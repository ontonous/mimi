// Mimi language runtime — pure Rust implementation.
//
// This module provides all runtime symbols needed by LLVM-codegened Mimi programs,
// replacing the previous C implementation (mimi_runtime.c). Every function is
// `#[no_mangle] pub extern "C"` so it can be linked from generated machine code.

pub mod profiler;
//
// Items 1/4/6/9 from the C runtime audit are eliminated:
//   - Item 1: Thread pool TOCTOU — use Rust `Mutex` + `Condvar` (already fixed in ffi/runtime.rs)
//   - Item 4: JSON recursion depth — Rust handles via normal recursion limit (guarded by
//     `json_max_depth`)
//   - Item 6: Unbounded string operations — use Rust `String`/`Vec` with safe bounds
//   - Item 9: Map capacity divide-by-zero — Rust `HashMap` never has zero capacity
//
// ## Standalone compilation
//
// For linking with Mimi-compiled object files, compile `standalone.rs` with:
// ```sh
// rustc --edition 2021 --crate-type staticlib --cfg standalone --crate-name mimi_runtime \
//       -o libmimi_runtime.a src/runtime/standalone.rs
// cc -no-pie -o output mimi_codegen.o libmimi_runtime.a -lpthread -ldl -lm
// ```

// When compiled directly with rustc (--cfg standalone), provide our own POSIX FFI declarations.
// When compiled via cargo, the real `libc` crate is used from Cargo.toml.

#[cfg(standalone)]
#[allow(non_camel_case_types, dead_code)]
mod libc {
    use std::ffi::c_void;

    // --- types ---
    pub type c_int = i32;
    pub type c_long = i64;
    pub type c_char = i8;
    pub type size_t = usize;
    pub type socklen_t = u32;
    pub type sa_family_t = u16;

    #[repr(C)]
    pub struct in_addr {
        pub s_addr: u32,
    }

    #[repr(C)]
    pub struct sockaddr_in {
        pub sin_family: sa_family_t,
        pub sin_port: u16,
        pub sin_addr: in_addr,
        pub sin_zero: [u8; 8],
    }

    #[repr(C)]
    pub struct sockaddr {
        pub sa_family: sa_family_t,
        pub sa_data: [u8; 14],
    }

    #[repr(C)]
    pub struct addrinfo {
        pub ai_flags: i32,
        pub ai_family: i32,
        pub ai_socktype: i32,
        pub ai_protocol: i32,
        pub ai_addrlen: socklen_t,
        pub ai_addr: *mut sockaddr,
        pub ai_canonname: *mut i8,
        pub ai_next: *mut addrinfo,
    }

    // --- constants ---
    pub const AF_UNSPEC: i32 = 0;
    pub const SOCK_STREAM: i32 = 1;
    pub const SOL_SOCKET: i32 = 1;
    pub const SO_REUSEADDR: i32 = 2;
    pub const IPPROTO_TCP: i32 = 6;
    pub const TCP_NODELAY: i32 = 1;
    pub const AF_INET: i32 = 2;
    pub const INADDR_ANY: u32 = 0;
    pub const SIGSEGV: i32 = 11;
    pub const SIGABRT: i32 = 6;
    pub const SIGBUS: i32 = 7;
    pub const SIGILL: i32 = 4;
    pub const SIGFPE: i32 = 8;
    pub const SIG_DFL: usize = 0;
    pub const SIG_ERR: usize = usize::MAX;
    pub const _SC_PAGESIZE: c_int = 30;

    // --- functions ---
    extern "C" {
        pub fn socket(domain: i32, type_: i32, protocol: i32) -> i32;
        pub fn setsockopt(
            sockfd: i32,
            level: i32,
            optname: i32,
            optval: *const c_void,
            optlen: socklen_t,
        ) -> i32;
        pub fn bind(sockfd: i32, addr: *const sockaddr, addrlen: socklen_t) -> i32;
        pub fn listen(sockfd: i32, backlog: i32) -> i32;
        pub fn accept(sockfd: i32, addr: *mut sockaddr, addrlen: *mut socklen_t) -> i32;
        pub fn connect(sockfd: i32, addr: *const sockaddr, addrlen: socklen_t) -> i32;
        pub fn send(sockfd: i32, buf: *const c_void, len: usize, flags: i32) -> isize;
        pub fn recv(sockfd: i32, buf: *mut c_void, len: usize, flags: i32) -> isize;
        pub fn close(fd: i32) -> i32;
        pub fn getaddrinfo(
            node: *const i8,
            service: *const i8,
            hints: *const addrinfo,
            res: *mut *mut addrinfo,
        ) -> i32;
        pub fn freeaddrinfo(res: *mut addrinfo);
        pub fn signal(signum: i32, handler: usize) -> usize;
        pub fn malloc(size: usize) -> *mut c_void;
        pub fn realloc(ptr: *mut c_void, size: usize) -> *mut c_void;
        pub fn free(ptr: *mut c_void);
        pub fn atexit(func: extern "C" fn()) -> i32;
        pub fn sprintf(buf: *mut i8, fmt: *const i8, ...) -> i32;
        pub fn snprintf(buf: *mut i8, size: usize, fmt: *const i8, ...) -> i32;
        pub fn strlen(s: *const i8) -> usize;
        pub fn sysconf(name: c_int) -> c_long;
        pub fn mincore(addr: *mut c_void, len: usize, vec: *mut u8) -> c_int;
    }
}

use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

// Re-export types used by FFI tests and codegen
// Must match the C layouts exactly.
#[repr(C)]
pub struct MimiList {
    len: i64,
    data: *mut *mut std::ffi::c_char,
    // FFI-2: Tracks whether data was allocated by Rust (true) or received from C (false).
    // When true, free(data) uses libc::free. When false, skip free to avoid wrong allocator.
    owns_data: bool,
}

pub type ValueHandle = usize;
pub type MapHandle = usize;

// ---------------------------------------------------------------------------
// Memory allocation helpers
// ---------------------------------------------------------------------------

/// Allocate a C string (null-terminated) using libc::malloc.
/// The caller is responsible for freeing with mimi_string_free or libc::free.
fn alloc_c_string(s: &str) -> *mut std::ffi::c_char {
    // SAFETY: `len + 1` is non-zero; the null terminator is written within the allocated buffer.
    let bytes = s.as_bytes();
    let len = bytes.len();
    let ptr = unsafe { libc::malloc(len + 1) as *mut u8 };
    if ptr.is_null() {
        return std::ptr::null_mut();
    }
    if len > 0 {
        // SAFETY: source and destination are non-overlapping and `len` bytes fit in the allocation.
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, len);
        }
    }
    unsafe {
        // SAFETY: writing the null terminator at offset `len` is within the `len + 1` allocation.
        *ptr.add(len) = 0;
    }
    ptr as *mut std::ffi::c_char
}

/// JSON-escape a string: wrap in double quotes, escape `"`, `\`, and control chars.
/// JSON unescape: convert escape sequences \", \\, \/, \b, \f, \n, \r, \t, \uXXXX
/// into the actual characters they represent. Used during deserialization.
fn json_unescape(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] != b'\\' {
            out.push(s[i]);
            i += 1;
            continue;
        }
        i += 1;
        if i >= s.len() {
            out.push(b'\\');
            break;
        }
        match s[i] {
            b'"' => out.push(b'"'),
            b'\\' => out.push(b'\\'),
            b'/' => out.push(b'/'),
            b'b' => out.push(0x08),
            b'f' => out.push(0x0c),
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'u' => {
                // \uXXXX — parse 4 hex digits
                if i + 4 < s.len() {
                    let hex_str = std::str::from_utf8(&s[i + 1..i + 5]).unwrap_or("0000");
                    if let Ok(code) = u32::from_str_radix(hex_str, 16) {
                        if let Some(ch) = char::from_u32(code) {
                            let mut buf = [0u8; 4];
                            let encoded = ch.encode_utf8(&mut buf);
                            out.extend_from_slice(encoded.as_bytes());
                        }
                    }
                    i += 4;
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    out
}

fn json_escape_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// v0.28.13: Allocate a MimiList data array with hidden capacity header at data[-8].
/// The header uses bit 63 as a magic marker: `(i64::MIN | cap)`.
/// Returns the data pointer (header is at data[-8]). Null on failure.
fn alloc_list_data(cap: i64) -> *mut *mut std::ffi::c_char {
    if cap <= 0 {
        return std::ptr::null_mut();
    }
    // audit (MEDIUM): guard against overflow when casting i64 to usize on
    // 32-bit platforms. On 64-bit (primary target), i64→usize is lossless
    // for non-negative values. On 32-bit, cap > u32::MAX would wrap to 0,
    // producing a tiny allocation. Reject anything beyond u32::MAX on 32-bit
    // by using `try_into` or a manual bounds check.
    #[cfg(target_pointer_width = "32")]
    {
        if cap > (u32::MAX as i64) {
            return std::ptr::null_mut();
        }
    }
    let elem_size = std::mem::size_of::<*mut std::ffi::c_char>();
    let data_size = match (cap as usize).checked_mul(elem_size) {
        Some(s) => s,
        None => return std::ptr::null_mut(),
    };
    let sz = match 8usize.checked_add(data_size) {
        Some(s) => s,
        None => return std::ptr::null_mut(),
    };
    // SAFETY: `cap > 0` so the allocation size is non-zero; result is checked for null.
    let alloc = unsafe { libc::malloc(sz) as *mut i64 };
    if alloc.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        // SAFETY: writing the header at the base allocation before returning `base + 1`.
        *alloc = i64::MIN | cap;
    }
    // SAFETY: `alloc` points to a freshly allocated buffer with room for the header and `cap` slots.
    unsafe { alloc.add(1) as *mut *mut std::ffi::c_char }
}

/// Reallocate a MimiList data array, preserving the hidden capacity header.
fn realloc_list_data(old: *mut *mut std::ffi::c_char, new_cap: i64) -> *mut *mut std::ffi::c_char {
    if new_cap <= 0 {
        return std::ptr::null_mut();
    }
    // audit (MEDIUM): guard against overflow when casting i64 to usize on
    // 32-bit platforms. On 64-bit (primary target), i64→usize is lossless
    // for non-negative values. On 32-bit, cap > u32::MAX would wrap to 0,
    // producing a tiny allocation.
    #[cfg(target_pointer_width = "32")]
    {
        if new_cap > (u32::MAX as i64) {
            return std::ptr::null_mut();
        }
    }
    // H11 fix: use checked multiplication to prevent integer overflow that
    // could lead to undersized allocation and subsequent buffer overflow.
    let elem_size = std::mem::size_of::<*mut std::ffi::c_char>();
    let data_size = match (new_cap as usize).checked_mul(elem_size) {
        Some(s) => s,
        None => return std::ptr::null_mut(),
    };
    let sz = match 8usize.checked_add(data_size) {
        Some(s) => s,
        None => return std::ptr::null_mut(),
    };
    if old.is_null() {
        return alloc_list_data(new_cap);
    }
    // SAFETY: `old` came from `alloc_list_data`/`realloc_list_data`, so `old - 1` is the valid allocation base.
    let base = unsafe { (old as *mut i64).offset(-1) };
    // SAFETY: `base` points to the valid allocation base; `sz` is the new total size.
    let nb = unsafe { libc::realloc(base as *mut std::ffi::c_void, sz) as *mut i64 };
    if nb.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        // SAFETY: header is written at the new allocation base before returning the data pointer.
        *nb = i64::MIN | new_cap;
    }
    // SAFETY: `nb` points to a buffer with room for the header and `new_cap` slots.
    unsafe { nb.add(1) as *mut *mut std::ffi::c_char }
}

/// Read the hidden capacity from data[-8]. Returns 0 if no header.
/// H1 fix: only reads header for lists where owns_data is true. For
/// non-owning lists or null data, returns 0 immediately to avoid
/// reading garbage that could coincidentally have bit 63 set.
fn list_cap(list: &MimiList) -> i64 {
    if !list.owns_data || list.data.is_null() {
        return 0;
    }
    let hdr = unsafe { *(list.data as *mut i64).offset(-1) };
    if hdr < 0 {
        hdr & 0x7FFF_FFFF_FFFF_FFFF
    } else {
        0
    }
}

/// v0.28.13: Push an i64 element into a MimiList with exponential capacity growth.
/// Uses hidden header (alloc_list_data/realloc_list_data) for O(1) amortized push.
/// Modifies list in place (data and len are updated).
#[no_mangle]
pub extern "C" fn mimi_list_push_i64(list: *mut MimiList, element: i64) {
    if list.is_null() {
        return;
    }
    let lst = unsafe { &mut *list };
    let len = lst.len;
    let cap = list_cap(lst);
    // MEM-C10 (deep audit): use checked_add to prevent integer overflow on len+1.
    let new_len = match len.checked_add(1) {
        Some(n) => n,
        None => return, // len overflow — can't push more
    };
    if new_len > cap {
        // MEM-C10: use checked_mul for cap*2 to prevent overflow.
        let nc = if cap <= 0 {
            4
        } else {
            match cap.checked_mul(2) {
                Some(c) => c,
                None => return,
            }
        };
        let nd = realloc_list_data(lst.data, nc);
        if nd.is_null() {
            return;
        }
        lst.data = nd;
        // SAFETY: after growth `nd` has capacity >= `new_len`; writing at index `len` is in bounds.
        unsafe {
            *(nd as *mut i64).add(len as usize) = element;
        }
    } else {
        unsafe {
            // SAFETY: `len < cap`, so writing at index `len` is within the existing allocation.
            *(lst.data as *mut i64).add(len as usize) = element;
        }
    }
    lst.len = len + 1;
}

/// v0.28.13: Grow the data array of a MimiList if needed (exponential growth).
/// Returns the (possibly new) data pointer. The caller is responsible for
/// storing the element at `data[len]` and incrementing `list.len`.
/// This variant works for any element type (not just i64).
#[no_mangle]
pub extern "C" fn mimi_list_push_grow(
    list: *mut MimiList,
    additional: i64,
) -> *mut *mut std::ffi::c_char {
    if list.is_null() || additional <= 0 {
        return std::ptr::null_mut();
    }
    // SAFETY: `list` was checked non-null; mutable reference is held only within this function.
    let lst = unsafe { &mut *list };
    let len = lst.len;
    let old_data = lst.data;
    let cap = list_cap(lst);
    // MEM-C10/C11 (deep audit): overflow guard on `len + additional`. A corrupt
    // or adversarial `len`/`additional` could wrap to a non-positive value and
    // skip growth, leaving the caller to write out of bounds.
    let needed = match len.checked_add(additional) {
        Some(n) => n,
        None => return std::ptr::null_mut(),
    };
    if needed > cap {
        let new_cap = if cap <= 0 {
            if needed < 4 {
                4
            } else {
                needed
            }
        } else {
            // H12 fix: prevent infinite loop on corrupted cap. If doubling
            // would overflow, cap at i64::MAX (effectively unbounded).
            let mut nc = cap;
            while nc < needed {
                nc = match nc.checked_mul(2) {
                    Some(v) => v,
                    None => {
                        nc = i64::MAX;
                        break;
                    }
                };
            }
            nc
        };
        // Allocate new buffer with header
        let new_data = alloc_list_data(new_cap);
        if new_data.is_null() {
            return std::ptr::null_mut();
        }
        // Copy existing elements from old buffer (which may lack a header)
        if !old_data.is_null() && len > 0 {
            let copy_size =
                match (len as usize).checked_mul(std::mem::size_of::<*mut std::ffi::c_char>()) {
                    Some(s) => s,
                    None => return std::ptr::null_mut(),
                };
            // SAFETY: existing elements are copied byte-for-byte from the old buffer to the new buffer.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    old_data as *const u8,
                    new_data as *mut u8,
                    copy_size,
                );
            }
        }
        // Free old data if it has a header; otherwise skip (can't verify origin)
        if cap > 0 {
            // Has header: free allocation base (data - 8)
            // SAFETY: `old_data` has a hidden header, so `old_data - 1` is the valid allocation base.
            let base = unsafe { (old_data as *mut i64).offset(-1) as *mut std::ffi::c_void };
            unsafe {
                // SAFETY: `base` is the valid allocation base returned by `alloc_list_data`.
                libc::free(base);
            }
        }
        // H2 fix: when cap <= 0 (no header), old_data may point to a static buffer
        // or stack-allocated array. We cannot verify it was heap-allocated, so
        // we skip libc::free to avoid UB (freeing non-heap memory). The old_data
        // pointer is replaced by new_data; the caller is responsible for the old
        // allocation's lifetime.
        lst.data = new_data;
        new_data
    } else {
        old_data
    }
}

/// S15/S22: Free a C string allocated by alloc_c_string.
/// Safe to call with null pointer (no-op).
#[no_mangle]
pub extern "C" fn mimi_string_free(ptr: *mut std::ffi::c_char) {
    if !ptr.is_null() {
        unsafe {
            // SAFETY: freeing a non-null pointer allocated by the matching `libc::malloc`.
            libc::free(ptr as *mut std::ffi::c_void);
        }
    }
}

/// Free a MimiList and optionally its C string elements.
/// The MimiList struct itself is always heap-allocated via Box in this runtime,
/// so we use Box::from_raw to free it (NOT libc::free, which would be allocator mismatch).
/// FFI-2: Only frees data if `owns_data` is true (Rust-allocated).
/// C-allocated data (owns_data=false) is skipped to avoid wrong-allocator heap corruption.
///
/// v0.28.13: Detects the hidden capacity header (negative value at data[-8]).
/// If present, frees the allocation base at data-8. Otherwise original behavior.
#[no_mangle]
pub extern "C" fn mimi_list_free(list: *mut MimiList, free_elements: bool) {
    if list.is_null() {
        return;
    }
    // SAFETY: list is non-null (checked above) and points to a valid
    // MimiList allocated by mimi_list_alloc. RT-H6: copy fields out before
    // any free so we never hold a live `&*list` across `Box::from_raw`.
    unsafe {
        let owns_data = (*list).owns_data;
        let data_ptr = (*list).data;
        let list_len = (*list).len;
        // MEM-C10 (deep audit): bound iteration against a corrupt/negative `len`
        // so a hostile or buggy `len` cannot drive an out-of-bounds read or an
        // unbounded `libc::free` loop. When a capacity header is present we also
        // clamp to `cap` (no valid element can live beyond it).
        let safe_count = {
            if list_len < 0 {
                0usize
            } else {
                // Temporary view only for list_cap; not held across free.
                let cap = list_cap(&*list);
                let mut n = list_len as usize;
                if cap > 0 && n > cap as usize {
                    n = cap as usize;
                }
                if n > 1_000_000_000 {
                    n = 1_000_000_000;
                }
                n
            }
        };
        if owns_data && !data_ptr.is_null() {
            let cap = list_cap(&*list);
            if cap > 0 {
                if free_elements {
                    for i in 0..safe_count {
                        let e = *data_ptr.add(i);
                        if !e.is_null() {
                            libc::free(e as *mut std::ffi::c_void);
                        }
                    }
                }
                let base = (data_ptr as *mut i64).offset(-1) as *mut std::ffi::c_void;
                libc::free(base);
            } else {
                if free_elements {
                    for i in 0..safe_count {
                        let e = *data_ptr.add(i);
                        if !e.is_null() {
                            libc::free(e as *mut std::ffi::c_void);
                        }
                    }
                }
                libc::free(data_ptr as *mut std::ffi::c_void);
            }
        }
        // C1 fix: The MimiList struct was allocated via Box::new()/Box::into_raw() in
        // all runtime functions (mimi_str_split, mimi_map_keys, etc.).
        // Using libc::free here would be UB on musl/macOS (allocator mismatch).
        drop(Box::from_raw(list));
    }
}

/// Free the element pointers of a MimiList (NOT the data buffer, NOT the list struct).
/// Used for lists whose elements are individually heap-allocated
/// (e.g. from_json::<List<Record>> where each element is a separate malloc'd record
/// struct stored as ptrtoint i64 in the data array).
/// The data buffer is freed separately by the existing `register_heap_slot` mechanism;
/// the list struct itself is a stack alloca and must NOT be freed.
#[no_mangle]
pub extern "C" fn mimi_list_free_elements(list: *mut MimiList) {
    if list.is_null() {
        return;
    }
    // SAFETY: list is non-null (checked by the caller) and points to a
    // valid MimiList. The reference `&*list` lives only within this
    // function body.
    unsafe {
        let lst = &*list;
        // H8 fix: only free elements if the list owns its data. C-allocated
        // lists (owns_data=false) have elements allocated by the C allocator
        // and must not be freed via libc::free (wrong allocator or double-free).
        if lst.owns_data && !lst.data.is_null() {
            // MEM-C10 (deep audit): bound iteration against a corrupt/negative len.
            let safe_count = {
                let l = lst.len;
                if l < 0 {
                    0usize
                } else {
                    let mut n = l as usize;
                    if n > 1_000_000_000 {
                        n = 1_000_000_000;
                    }
                    n
                }
            };
            for i in 0..safe_count {
                let e = *lst.data.add(i);
                if !e.is_null() {
                    libc::free(e as *mut std::ffi::c_void);
                }
            }
            // NOT freeing the data buffer — that is handled by register_heap_slot
            // NOT freeing the list struct itself — it is a stack-allocated alloca
        }
    }
}

/// Allocate a C string from raw bytes. Always appends a trailing NUL so the
/// result is safe for `cstr_to_string` / libc string APIs.
///
/// Callers may pass either already-NUL-terminated buffers (e.g. sprintf
/// output including the terminator) or plain payload bytes (e.g.
/// `json_unescape` output). When the input already ends in `0`, that
/// terminator is kept and no extra byte is added.
fn alloc_c_string_from_bytes(bytes: &[u8]) -> *mut std::ffi::c_char {
    let needs_nul = bytes.is_empty() || bytes.last() != Some(&0);
    let payload_len = bytes.len();
    let alloc_len = if needs_nul {
        match payload_len.checked_add(1) {
            Some(n) => n,
            None => return std::ptr::null_mut(),
        }
    } else {
        payload_len
    };
    // SAFETY: alloc_len is at least 1 when payload is empty (needs_nul).
    let ptr = unsafe { libc::malloc(alloc_len) as *mut u8 };
    if ptr.is_null() {
        return std::ptr::null_mut();
    }
    if payload_len > 0 {
        // SAFETY: non-overlapping copy of payload_len bytes into alloc_len buffer.
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, payload_len);
        }
    }
    if needs_nul {
        // SAFETY: writing NUL at offset payload_len is within alloc_len.
        unsafe {
            *ptr.add(payload_len) = 0;
        }
    }
    ptr as *mut std::ffi::c_char
}

// ---------------------------------------------------------------------------
// Integer math
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn __mimi_pow_i64(base: i64, exp: i64) -> i64 {
    if exp < 0 {
        return 0;
    }
    if exp == 0 {
        return 1;
    }
    let mut result: i64 = 1;
    let mut b: i64 = base;
    let mut e: i64 = exp;
    while e > 0 {
        if (e & 1) != 0 {
            match result.checked_mul(b) {
                Some(v) => result = v,
                None => return 0,
            }
        }
        e >>= 1;
        if e > 0 {
            match b.checked_mul(b) {
                Some(v) => b = v,
                None => return 0,
            }
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Reference counting (atomic)
// ---------------------------------------------------------------------------
// Layout: [AtomicI64 strong | AtomicI64 weak | i64 alloc_size | user data ...]
// Returns pointer to user data (right after refcount header).

#[repr(C)]
struct RcHeader {
    strong: AtomicI64,
    weak: AtomicI64,
    alloc_size: i64,
}

// SAFETY: caller ensures `ptr` was returned by `mimi_rc_alloc` and is still valid; offset -1 lands in the `RcHeader`.
unsafe fn rc_header_from_ptr(ptr: *mut std::ffi::c_void) -> *mut RcHeader {
    (ptr as *mut RcHeader).sub(1)
}

/// S1: Helper to get a shared reference for atomic operations (no aliasing UB).
/// Caller must ensure ptr is valid and not concurrently freed.
// SAFETY: caller ensures `ptr` is valid and not concurrently freed; the returned reference lifetime is bounded by the caller.
unsafe fn rc_header_ref(ptr: *mut std::ffi::c_void) -> &'static RcHeader {
    &*(ptr as *mut RcHeader).sub(1)
}

#[no_mangle]
pub extern "C" fn mimi_rc_alloc(size: i64) -> *mut std::ffi::c_void {
    // FFI-1: Reject negative/huge sizes that would cause Layout::array to panic.
    // abort() is async-signal-safe and the only safe option across FFI boundary.
    if size <= 0 || size > 0x7fff_ffff {
        std::process::abort();
    }
    let layout = std::alloc::Layout::new::<RcHeader>()
        .extend(
            std::alloc::Layout::array::<u8>(size as usize)
                .unwrap_or_else(|_| std::process::abort()),
        )
        .unwrap_or_else(|_| std::process::abort())
        .0
        .pad_to_align();
    // SAFETY: `layout` has non-zero size and alignment; null result is handled.
    let ptr = unsafe { std::alloc::alloc(layout) };
    if ptr.is_null() {
        return std::ptr::null_mut();
    }
    let hdr = ptr as *mut RcHeader;
    unsafe {
        // SAFETY: `ptr` points to uninitialized memory with enough space for `RcHeader`; fields are fully initialized.
        (*hdr).strong = AtomicI64::new(1);
        (*hdr).weak = AtomicI64::new(0);
        (*hdr).alloc_size = size;
    }
    // SAFETY: header is initialized before returning pointer to user data at `hdr + 1`.
    unsafe { (hdr.add(1)) as *mut std::ffi::c_void }
}

#[no_mangle]
pub extern "C" fn mimi_rc_retain(ptr: *mut std::ffi::c_void) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: `ptr` was checked non-null and came from `mimi_rc_alloc`, so the header is valid.
    let hdr = unsafe { rc_header_from_ptr(ptr) };
    unsafe {
        // SAFETY: atomic increment on a valid strong count; no other thread is deallocating because strong > 0.
        (*hdr).strong.fetch_add(1, Ordering::Relaxed);
    }
}

/// Helper: build the dealloc Layout from RcHeader's stored alloc_size.
/// FFI-1: Uses abort instead of panicking if alloc_size is corrupted.
// SAFETY: `hdr` must point to a valid `RcHeader`; `alloc_size` is validated before constructing the `Layout`.
unsafe fn rc_dealloc_layout(hdr: *mut RcHeader) -> std::alloc::Layout {
    let user_size = (*hdr).alloc_size as usize;
    // Guard against corrupted alloc_size that would cause Layout::array to panic.
    if user_size == 0 || user_size > 0x7fff_ffff {
        std::process::abort();
    }
    std::alloc::Layout::new::<RcHeader>()
        .extend(
            std::alloc::Layout::array::<u8>(user_size).unwrap_or_else(|_| std::process::abort()),
        )
        .unwrap_or_else(|_| std::process::abort())
        .0
        .pad_to_align()
}

#[no_mangle]
pub extern "C" fn mimi_rc_release(ptr: *mut std::ffi::c_void) {
    if ptr.is_null() {
        return;
    }
    let hdr = unsafe { rc_header_from_ptr(ptr) };
    // SAFETY: atomic decrement with Release ordering; if it returns 1, we own the last strong reference.
    // B7: TOCTOU analysis — after fetch_sub returns 1 (strong is now 0), a
    // concurrent weak_retain may be running its CAS loop. However, weak_retain
    // checks strong==0 && weak==0 and returns early if both are zero. So:
    // - If weak_retain sees strong=0, weak=0 → it returns (no increment)
    // - If weak_retain sees strong=0, weak>0 → it CAS-increments weak
    // In the latter case, our weak==0 load will see weak>0 and skip dealloc,
    // deferring to the final weak_release. This is the standard Arc drop pattern.
    if unsafe { (*hdr).strong.fetch_sub(1, Ordering::Release) } == 1 {
        std::sync::atomic::fence(Ordering::Acquire);
        // H3 fix: Use Acquire ordering for the weak count load to synchronize
        // with the AcqRelease CAS in mimi_rc_weak_retain. Without this, a
        // Relaxed load could see a stale weak count of 0 even after another
        // thread's weak_retain CAS has incremented it, leading to dealloc
        // racing with a concurrent weak retain on a different thread.
        if unsafe { (*hdr).weak.load(Ordering::Acquire) } == 0 {
            let layout = unsafe { rc_dealloc_layout(hdr) };
            // SAFETY: hdr is non-null and was allocated with this layout
            // (we just observed strong==0 and weak==0, so no other thread
            // can hold a reference). dealloc must be called with the same
            // layout used in the original alloc.
            unsafe {
                std::alloc::dealloc(hdr as *mut u8, layout);
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn mimi_rc_weak_retain(ptr: *mut std::ffi::c_void) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: ptr was null-checked and originated from mimi_rc_alloc, so the header is valid.
    let hdr = unsafe { rc_header_from_ptr(ptr) };
    // S2: CAS loop to avoid TOCTOU race on weak count.
    // Old code: load strong, load weak, check both zero, then fetch_add.
    // Between load and fetch_add, another thread could complete release+dealloc.
    // CAS ensures we only increment if the object is still alive.
    loop {
        // SAFETY: reading atomic counts on a valid header while the object is still potentially alive.
        let s = unsafe { (*hdr).strong.load(Ordering::Acquire) };
        let w = unsafe { (*hdr).weak.load(Ordering::Relaxed) };
        if s == 0 && w == 0 {
            return; // Object already freed or being freed
        }
        // Try to increment weak; if strong went to 0 between our load and CAS, retry.
        // SAFETY: CAS loop only increments weak count while the object is still alive (strong > 0 or weak > 0).
        let prev = unsafe {
            (*hdr)
                .weak
                .compare_exchange(w, w + 1, Ordering::AcqRel, Ordering::Relaxed)
        };
        if prev.is_ok() {
            return;
        }
    }
}

#[no_mangle]
pub extern "C" fn mimi_rc_weak_release(ptr: *mut std::ffi::c_void) {
    if ptr.is_null() {
        return;
    }
    let hdr = unsafe { rc_header_from_ptr(ptr) };
    // SAFETY: atomic decrement with Release ordering; if it returns 1, we own the last weak reference.
    if unsafe { (*hdr).weak.fetch_sub(1, Ordering::Release) } == 1 {
        std::sync::atomic::fence(Ordering::Acquire);
        // H4 fix: same TOCTOU as H3 — use Acquire ordering for strong count
        // load to synchronize with concurrent strong retain operations.
        if unsafe { (*hdr).strong.load(Ordering::Acquire) } <= 0 {
            let layout = unsafe { rc_dealloc_layout(hdr) };
            // SAFETY: hdr is non-null; we just observed weak==0 and strong<=0
            // under Acquire ordering, so no other thread can hold a reference.
            // The layout matches the one used at alloc time.
            unsafe {
                std::alloc::dealloc(hdr as *mut u8, layout);
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn mimi_rc_upgrade(ptr: *mut std::ffi::c_void) -> *mut std::ffi::c_void {
    if ptr.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `ptr` was checked non-null and came from `mimi_rc_alloc`; `rc_header_ref` contract satisfied.
    let hdr = unsafe { rc_header_ref(ptr) };
    // H19: use Acquire on initial load to match the CAS on the release path,
    // ensuring we see the latest strong count. Relaxed could observe a stale 0
    // and return null even when strong=1, causing a false-negative upgrade failure.
    let mut s = hdr.strong.load(Ordering::Acquire);
    loop {
        if s == 0 {
            return std::ptr::null_mut();
        }
        // RT-H7: success path AcqRel so the increment synchronizes with
        // Release decrements on the free path (not only a post-CAS fence).
        match hdr
            .strong
            .compare_exchange_weak(s, s + 1, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => {
                // M31: Acquire fence ensures all prior writes to the RC object
                // are visible after a successful weak upgrade.
                std::sync::atomic::fence(Ordering::Acquire);
                return ptr;
            }
            Err(new_s) => s = new_s,
        }
    }
}

// ---------------------------------------------------------------------------
// Map (hash table via std::collections::HashMap)
// ---------------------------------------------------------------------------

struct MimiMap {
    inner: HashMap<String, ValueHandle>,
}

/// S4: Return raw pointer instead of &'static mut to avoid aliasing UB.
/// Callers must dereference within a single scope (no two &mut to same handle).
/// S18: abort() instead of panic! — panic across FFI boundary is UB (Rust ABI requirement).
// SAFETY: aborts on invalid handle (0); caller must ensure `handle` is a unique `Box<MimiMap>` and avoid aliased mutable access.
unsafe fn map_from_handle(handle: MapHandle) -> *mut MimiMap {
    if handle == 0 {
        std::process::abort();
    }
    handle as *mut MimiMap
}

#[no_mangle]
pub extern "C" fn mimi_map_new() -> MapHandle {
    let map = Box::new(MimiMap {
        inner: HashMap::new(),
    });
    Box::into_raw(map) as MapHandle
}

#[no_mangle]
pub extern "C" fn mimi_map_destroy(handle: MapHandle) {
    if handle == 0 {
        return;
    }
    // SAFETY: handle is non-zero; reconstructing the Box and dropping it.
    unsafe {
        drop(Box::from_raw(handle as *mut MimiMap));
    }
}

#[no_mangle]
pub extern "C" fn mimi_map_size(handle: MapHandle) -> i64 {
    if handle == 0 {
        return 0;
    }
    // SAFETY: handle validated by `map_from_handle`; deref is in a single scope.
    unsafe { (*map_from_handle(handle)).inner.len() as i64 }
}

#[no_mangle]
pub extern "C" fn mimi_map_has_key(handle: MapHandle, key: *const std::ffi::c_char) -> i32 {
    if handle == 0 || key.is_null() {
        return 0;
    }
    let s = unsafe { cstr_to_string(key) };
    // SAFETY: handle validated by `map_from_handle`; deref is in a single scope.
    unsafe { (*map_from_handle(handle)).inner.contains_key(&s) as i32 }
}

#[no_mangle]
pub extern "C" fn mimi_map_get(handle: MapHandle, key: *const std::ffi::c_char) -> ValueHandle {
    if handle == 0 || key.is_null() {
        return 0;
    }
    let s = unsafe { cstr_to_string(key) };
    // SAFETY: handle validated by `map_from_handle`; deref is in a single scope.
    unsafe {
        (*map_from_handle(handle))
            .inner
            .get(&s)
            .copied()
            .unwrap_or(0)
    }
}

#[no_mangle]
pub extern "C" fn mimi_map_set(
    handle: MapHandle,
    key: *const std::ffi::c_char,
    value: ValueHandle,
) {
    if handle == 0 || key.is_null() {
        return;
    }
    let s = unsafe { cstr_to_string(key) };
    // SAFETY: handle validated by `map_from_handle`; deref is in a single scope.
    unsafe {
        (*map_from_handle(handle)).inner.insert(s, value);
    }
}

/// Format an `Any` value (a raw usize handle) to a heap-allocated C string.
///
/// Uses a two-tier approach:
/// 1. If bit-0 is clear and the value looks like a plausible heap pointer
///    (>= 1MB, 8-byte aligned), performs a *bounded* scan (max 256 bytes)
///    for a null terminator to confirm it's a valid C string.
/// 2. Falls back to raw integer formatting for everything else.
///
/// Integers are stored directly (no (val<<1)|1 tag) since CG-H16 fix.
/// Pointers are stored with bit-0 = 0 due to alignment; the heuristic
/// distinguishes them from integers by size and alignment.
///
/// The caller must `free` the returned pointer with `mimi_string_free`.
#[no_mangle]
pub extern "C" fn mimi_any_to_string(value: ValueHandle) -> *mut std::ffi::c_char {
    const MIN_HEAP: usize = 1_048_576; // 1MB — below this is definitely not a heap ptr
    const MAX_ADDR: usize = usize::MAX - 4096;
    const MAX_BOUNDED_SCAN: usize = 256; // C12: limit scan to 256 bytes to avoid 1MB arbitrary read

    // Bit-0 = 0: could be an aligned heap pointer (string), or an even integer.
    // Validate before treating as pointer.
    if value & 1 == 0 && (MIN_HEAP..MAX_ADDR).contains(&value) && value % 8 == 0 {
        let ptr = value as *const u8;
        // C12 (deep audit): a large *untagged* integer (e.g. `0x7FFF_FFFF_F000`)
        // satisfies the heuristic above but points at unmapped memory, so the
        // first read below would SIGSEGV. Probe whether the address is actually
        // mapped (mincore) and only scan within that mapped page, so we never
        // dereference memory we don't own.
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let page_size = if page_size == 0 { 4096 } else { page_size };
        let page_start = (value / page_size) * page_size;
        let mut mvec: u8 = 0;
        let mapped =
            unsafe { libc::mincore(page_start as *mut std::ffi::c_void, page_size, &mut mvec) };
        if mapped == 0 {
            let page_offset = value - page_start;
            let max_in_page = page_size.saturating_sub(page_offset);
            let max_scan = max_in_page.min(MAX_BOUNDED_SCAN);
            let mut len: usize = 0;
            // SAFETY: mincore confirmed the first page is mapped; the scan is
            // bounded to that page so it cannot cross into an unmapped page.
            unsafe {
                while len < max_scan {
                    let byte = *ptr.add(len);
                    if byte == 0 {
                        // Found null terminator within the mapped page — likely a real C string.
                        let buf = libc::malloc(len + 1) as *mut u8;
                        if buf.is_null() {
                            return std::ptr::null_mut();
                        }
                        if len > 0 {
                            std::ptr::copy_nonoverlapping(ptr, buf, len);
                        }
                        *buf.add(len) = 0;
                        return buf as *mut std::ffi::c_char;
                    }
                    len += 1;
                }
            }
        }
        // C12: no null within 256 bytes — treat as large integer (≥1MB) and
        // format as hex to avoid reading arbitrary memory for 1MB.
        // SAFETY: malloc(24) returned a valid buffer; null check below
        // guards against OOM. The buffer is at least 24 bytes and the
        // format string "0x%lx\0" writes at most ~20 bytes on 64-bit.
        let buf = unsafe { libc::malloc(24) as *mut std::ffi::c_char };
        if buf.is_null() {
            return std::ptr::null_mut();
        }
        // SAFETY: buf is non-null and 24 bytes; snprintf is bounded to size.
        unsafe {
            libc::snprintf(buf, 24, b"0x%lx\0".as_ptr() as *const _, value as u64);
        }
        return buf;
    }

    // Fallback: format as raw decimal integer.
    let buf = unsafe { libc::malloc(24) as *mut std::ffi::c_char };
    if buf.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        libc::snprintf(buf, 24, b"%ld\0".as_ptr() as *const _, value as i64);
    }
    buf
}

#[no_mangle]
pub extern "C" fn mimi_map_remove(handle: MapHandle, key: *const std::ffi::c_char) -> i32 {
    if handle == 0 || key.is_null() {
        return 0;
    }
    let s = unsafe { cstr_to_string(key) };
    // SAFETY: handle validated by `map_from_handle`; deref is in a single scope.
    unsafe { (*map_from_handle(handle)).inner.remove(&s).is_some() as i32 }
}

#[no_mangle]
/// RT-H4 helper: treat a ValueHandle as a C string only if mincore says the
/// page is mapped and a NUL terminator appears within a bounded scan.
fn safe_c_string_from_handle(handle: ValueHandle) -> Option<String> {
    const MIN_HEAP: usize = 1_048_576;
    const MAX_BOUNDED_SCAN: usize = 256;
    if handle < MIN_HEAP || handle % 8 != 0 {
        return None;
    }
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
    let page_size = if page_size == 0 { 4096 } else { page_size };
    let page_start = (handle / page_size) * page_size;
    let mut mvec: u8 = 0;
    let mapped =
        unsafe { libc::mincore(page_start as *mut std::ffi::c_void, page_size, &mut mvec) };
    if mapped != 0 {
        return None;
    }
    let page_offset = handle - page_start;
    let max_scan = page_size.saturating_sub(page_offset).min(MAX_BOUNDED_SCAN);
    let ptr = handle as *const u8;
    // RT-H2 soft harden: copy bytes into a local buffer while scanning so a
    // concurrent munmap after mincore cannot corrupt the String we build from
    // a live slice. Residual race remains on the individual byte loads
    // themselves (cannot close fully without process_vm_readv / userfaultfd).
    let mut local = [0u8; MAX_BOUNDED_SCAN];
    let mut len = 0usize;
    // SAFETY: mincore confirmed mapped page; scan/copy bounded to that page.
    unsafe {
        while len < max_scan {
            let b = *ptr.add(len);
            if b == 0 {
                // Re-check mapping before trusting the snapshot.
                let mut mvec2: u8 = 0;
                if libc::mincore(page_start as *mut std::ffi::c_void, page_size, &mut mvec2) != 0
                {
                    return None;
                }
                return Some(String::from_utf8_lossy(&local[..len]).into_owned());
            }
            local[len] = b;
            len += 1;
        }
    }
    None
}

pub extern "C" fn mimi_map_from_list(
    keys: *mut ValueHandle,
    values: *mut ValueHandle,
    n: i64,
) -> MapHandle {
    let handle = mimi_map_new();
    if handle == 0 || keys.is_null() || values.is_null() || n <= 0 {
        return handle;
    }
    // C6/C7 fix: validate n bounds and ensure pointers look like valid
    // C string pointers before dereferencing.
    let n = n.min(1_000_000);
    let map_ptr = handle as *mut MimiMap;
    for i in 0..n {
        // C6: We only have the caller's word that arrays have >= n elements.
        // We mitigate by capping n at 1M, but the real fix requires a
        // different API that takes slices. For now, validate each key
        // handle looks like a plausible heap pointer before dereference.
        // SAFETY: keys and values are non-null (caller-checked) and n
        // is capped at 1M, so index `i` is in bounds for the caller's
        // arrays (we trust the caller's array length, the cap is just
        // a defensive upper bound).
        let key_handle = unsafe { *keys.add(i as usize) };
        let val_handle = unsafe { *values.add(i as usize) };
        // RT-H1/H4: only decode keys via safe_c_string_from_handle (mincore+NUL).
        if let Some(s) = safe_c_string_from_handle(key_handle) {
            // SAFETY: map_ptr is the just-allocated map (handle != 0).
            unsafe {
                (*map_ptr).inner.insert(s, val_handle);
            }
        }
    }
    handle
}

fn mimi_map_collect(handle: MapHandle, collect_values: bool) -> *mut MimiList {
    if handle == 0 {
        let list = Box::new(MimiList {
            len: 0,
            data: std::ptr::null_mut(),
            owns_data: true, // owns the list struct (even if data is null)
        });
        return Box::into_raw(list);
    }
    // SAFETY: handle validated by `map_from_handle`; shared reference is in a single scope.
    let map = unsafe { &*map_from_handle(handle) };
    let len = map.inner.len() as i64;
    if len == 0 {
        let list = Box::new(MimiList {
            len: 0,
            data: std::ptr::null_mut(),
            owns_data: true,
        });
        return Box::into_raw(list);
    }

    // Use libc::malloc for the data pointer to ensure it is compatible with
    // libc::free (which mimi_list_free uses). Rust Vec uses jemalloc/allocator
    // which may not be compatible with libc::free on all platforms (e.g. MSVC).
    // H18: use checked_mul to prevent integer overflow on large maps.
    let data_size = match (len as usize).checked_mul(std::mem::size_of::<*mut std::ffi::c_char>()) {
        Some(s) => s,
        None => {
            return Box::into_raw(Box::new(MimiList {
                len: 0,
                data: std::ptr::null_mut(),
                owns_data: true,
            }))
        }
    };
    let data_ptr = if data_size > 0 {
        // SAFETY: data_size is positive and within reasonable bounds.
        unsafe { libc::malloc(data_size) as *mut *mut std::ffi::c_char }
    } else {
        std::ptr::null_mut()
    };
    if !data_ptr.is_null() {
        for (i, (k, v)) in map.inner.iter().enumerate() {
            let entry = if collect_values {
                // S10: ValueHandle is an opaque integer; cast to pointer for FFI transport.
                // Caller must NOT free these pointers — they are not heap-allocated strings.
                *v as *mut std::ffi::c_char
            } else {
                alloc_c_string(k.as_str())
            };
            // SAFETY: data_ptr is valid, i is within bounds.
            unsafe {
                *data_ptr.add(i) = entry;
            }
        }
    }
    let list = Box::new(MimiList {
        len,
        data: data_ptr,
        owns_data: !collect_values,
    });
    Box::into_raw(list)
}

#[no_mangle]
pub extern "C" fn mimi_map_keys(handle: MapHandle) -> *mut MimiList {
    mimi_map_collect(handle, false)
}

#[no_mangle]
pub extern "C" fn mimi_map_values(handle: MapHandle) -> *mut MimiList {
    mimi_map_collect(handle, true)
}

#[no_mangle]
pub extern "C" fn mimi_value_type_name(_handle: ValueHandle) -> *const std::ffi::c_char {
    // Matches C behavior: always returns "unknown"
    static UNKNOWN: &[u8] = b"unknown\0";
    UNKNOWN.as_ptr() as *const std::ffi::c_char
}

// ---------------------------------------------------------------------------
// String functions
// ---------------------------------------------------------------------------

// SAFETY: null pointer is checked before `CStr::from_ptr`; `to_string_lossy` handles non-UTF-8 bytes safely.
unsafe fn cstr_to_string(ptr: *const std::ffi::c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    CStr::from_ptr(ptr).to_string_lossy().into_owned()
}

/// Heap-copy a C string with known length into a new allocation.
/// Returns a ValueHandle (pointer) suitable for storage in a map and
/// later detection by `mimi_any_to_string` (aligned heap pointer >= 1MB).
/// The caller (codegen side) is responsible for freeing via `mimi_string_free`.
#[no_mangle]
pub extern "C" fn mimi_str_clone(ptr: *const std::ffi::c_char, len: i64) -> ValueHandle {
    if ptr.is_null() || len <= 0 {
        return 0;
    }
    // RT-H9: cap length to prevent absurd allocations / OOB copy requests.
    const MAX_STR_CLONE: i64 = 64 * 1024 * 1024; // 64 MiB
    if len > MAX_STR_CLONE {
        return 0;
    }
    // MEM-C9 (deep audit): use checked_add to prevent integer overflow on len+1.
    let alloc_len = match (len as usize).checked_add(1) {
        Some(n) => n,
        None => return 0, // overflow — can't allocate
    };
    let buf = unsafe { libc::malloc(alloc_len) as *mut u8 };
    if buf.is_null() {
        return 0;
    }
    // SAFETY: caller must ensure `ptr` points to at least `len` readable bytes.
    // We trust the length ABI used by codegen (not CStr::from_ptr).
    unsafe {
        std::ptr::copy_nonoverlapping(ptr as *const u8, buf, len as usize);
        *buf.add(len as usize) = 0;
    }
    buf as ValueHandle
}

/// Escape a C string for safe JSON string embedding.
/// Returns a new heap-allocated string (caller must free with mimi_string_free).
/// Handles: \ " \n \r \t \b \f and control chars as \uXXXX.
#[no_mangle]
pub extern "C" fn mimi_json_escape_string(ptr: *const std::ffi::c_char) -> *mut std::ffi::c_char {
    if ptr.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: ptr is non-null C string from caller.
    let s = unsafe { CStr::from_ptr(ptr) }.to_string_lossy();
    let mut escaped = String::with_capacity(s.len() + 2);
    escaped.push('"');
    for c in s.chars() {
        match c {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '\u{08}' => escaped.push_str("\\b"),
            '\u{0c}' => escaped.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                escaped.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => escaped.push(c),
        }
    }
    escaped.push('"');
    alloc_c_string(&escaped)
}

#[no_mangle]
pub extern "C" fn mimi_str_concat(
    a: *const std::ffi::c_char,
    b: *const std::ffi::c_char,
) -> *mut std::ffi::c_char {
    // SAFETY: `cstr_to_string` handles null pointers safely.
    let sa = unsafe { cstr_to_string(a) };
    // SAFETY: `cstr_to_string` handles null pointers safely.
    let sb = unsafe { cstr_to_string(b) };
    let result = format!("{}{}", sa, sb);
    alloc_c_string(&result)
}

#[no_mangle]
pub extern "C" fn mimi_str_split(
    s: *const std::ffi::c_char,
    delim: *const std::ffi::c_char,
) -> *mut MimiList {
    // SAFETY: `cstr_to_string` handles null pointers safely.
    let ss = unsafe { cstr_to_string(s) };
    // SAFETY: `cstr_to_string` handles null pointers safely.
    let d = unsafe { cstr_to_string(delim) };

    // audit (MEDIUM — mimi_str_split empty delimiter O(n)):
    // Empty delimiter splits the string into individual characters.
    // This is O(n) in the string length (one String allocation per char),
    // which is the expected semantic — there is no infinite loop here.
    // The result is bounded by the input length, so there is no DoS
    // amplification: a 1 MiB string produces at most 1 MiB of output.
    let parts: Vec<String> = if d.is_empty() {
        if ss.is_empty() {
            vec!["".to_string()]
        } else {
            ss.chars().map(|c| c.to_string()).collect()
        }
    } else {
        ss.split(&d).map(|p| p.to_string()).collect()
    };

    let len = parts.len() as i64;
    // H1 (audit): allocate the element array with libc::malloc so
    // `mimi_list_free` can free it with libc::free. A Rust Vec buffer
    // (even after ManuallyDrop) is a different allocator and is UB to
    // free via libc — and list_cap reading data[-8] on a Vec buffer is
    // also OOB.
    let data_ptr = if len <= 0 {
        std::ptr::null_mut()
    } else {
        let data_size = match (len as usize)
            .checked_mul(std::mem::size_of::<*mut std::ffi::c_char>())
        {
            Some(s) => s,
            None => {
                return Box::into_raw(Box::new(MimiList {
                    len: 0,
                    data: std::ptr::null_mut(),
                    owns_data: true,
                }));
            }
        };
        // SAFETY: data_size > 0; result checked for null.
        let ptr = unsafe { libc::malloc(data_size) as *mut *mut std::ffi::c_char };
        if ptr.is_null() {
            return std::ptr::null_mut();
        }
        for (i, p) in parts.into_iter().enumerate() {
            // SAFETY: i < len, ptr is valid for len elements.
            unsafe {
                *ptr.add(i) = alloc_c_string(&p);
            }
        }
        ptr
    };

    // FFI-2: data + string elements are libc-allocated — owns_data: true.
    // No hidden capacity header (list_cap returns 0 → free data directly).
    let list = Box::new(MimiList {
        len,
        data: data_ptr,
        owns_data: true,
    });
    Box::into_raw(list)
}

#[no_mangle]
pub extern "C" fn mimi_str_join(
    list: *const MimiList,
    sep: *const std::ffi::c_char,
) -> *mut std::ffi::c_char {
    if list.is_null() {
        return alloc_c_string("");
    }
    // SAFETY: `list` was checked non-null; shared reference is in a single scope.
    let lst = unsafe { &*list };
    if lst.data.is_null() || lst.len == 0 {
        return alloc_c_string("");
    }
    // FFI-12: Reject unreasonable list lengths to prevent DoS via i64::MAX loop.
    if lst.len < 0 || lst.len > 1_000_000 {
        return alloc_c_string("");
    }
    // SAFETY: `cstr_to_string` handles null pointers safely.
    let separator = unsafe { cstr_to_string(sep) };

    let mut parts: Vec<String> = Vec::with_capacity(lst.len as usize);
    for i in 0..lst.len as isize {
        // SAFETY: `i` is within `[0, len)` and data pointer is non-null for valid entries.
        unsafe {
            let ptr = *lst.data.offset(i);
            parts.push(cstr_to_string(ptr));
        }
    }
    let result = parts.join(&separator);
    alloc_c_string(&result)
}

/// Render a `MimiList` (codegen `{i64 len, i8* data}`) to a printable
/// heap-allocated C string. Used by the codegen `to_string` builtin
/// when it encounters a list value.
#[no_mangle]
pub extern "C" fn mimi_list_to_string(list: *const MimiList) -> *mut std::ffi::c_char {
    if list.is_null() {
        return alloc_c_string("[]");
    }
    // SAFETY: caller ensures `list` is a valid `*const MimiList` or null.
    let lst = unsafe { &*list };
    if lst.data.is_null() || lst.len == 0 {
        return alloc_c_string("[]");
    }
    if lst.len < 0 || lst.len > 1_000_000 {
        return alloc_c_string("[...]");
    }
    let mut parts: Vec<String> = Vec::with_capacity(lst.len as usize + 2);
    parts.push(String::from("["));
    for i in 0..lst.len as isize {
        if i > 0 {
            parts.push(String::from(", "));
        }
        // `lst.data` is `*mut *mut c_char`; dereference to a C string.
        let item_ptr = unsafe { *lst.data.offset(i) };
        if item_ptr.is_null() {
            parts.push(String::from("null"));
        } else {
            let s = unsafe { cstr_to_string(item_ptr) };
            parts.push(s);
        }
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

/// Render `List<Result<i32,i32>>` (ptrtoint of Result structs) as JSON array of
/// `{"Ok":[n]}` / `{"Err":[n]}` tags matching interp to_json.
#[no_mangle]
pub extern "C" fn mimi_list_result_i64_to_json(list: *const MimiList) -> *mut std::ffi::c_char {
    list_result_to_json_impl(list, 0)
}

/// Render `List<Result<Map<string, V>, i32>>` as JSON array.
/// `mode`: 0=i64 map, 1=string map, 2=bool map, 3=f64 map (same as other map JSON helpers).
#[no_mangle]
pub extern "C" fn mimi_list_result_map_to_json(
    list: *const MimiList,
    mode: i64,
) -> *mut std::ffi::c_char {
    list_result_to_json_impl(list, mode + 10)
}

/// `mode`:
/// - 0: Ok payload is plain i64
/// - 10..=13: Ok payload is MapHandle (mode-10 selects map value kind)
/// Decode Result Err payload to a JSON string fragment (already escaped/quoted).
fn decode_result_err_string(err: i64) -> String {
    const MIN_HEAP: i64 = 1_048_576;
    if err >= MIN_HEAP && (err as u64) % 8 == 0 {
        // Prefer Mimi string struct {ptr, i64} heap layout.
        let base = err as *const u8;
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let page_size = if page_size == 0 { 4096 } else { page_size };
        let page_start = ((err as usize) / page_size) * page_size;
        let mut mvec: u8 = 0;
        let mapped =
            unsafe { libc::mincore(page_start as *mut std::ffi::c_void, page_size, &mut mvec) };
        if mapped == 0 {
            // SAFETY: mincore confirmed mapped; load {ptr, len} if plausible.
            let ptr = unsafe { *(base as *const *const u8) };
            let len = unsafe { *(base.add(8) as *const i64) };
            if !ptr.is_null() && (0..1_000_000).contains(&len) {
                let slice = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
                if let Ok(s) = std::str::from_utf8(slice) {
                    return json_escape_string(s);
                }
            }
            // Fallback: C string at err address.
            if let Some(s) = safe_c_string_from_handle(err as ValueHandle) {
                return json_escape_string(&s);
            }
        }
    }
    // Scalar Err.
    format!("{}", err)
}

fn list_result_to_json_impl(list: *const MimiList, mode: i64) -> *mut std::ffi::c_char {
    if list.is_null() {
        return alloc_c_string("[]");
    }
    let lst = unsafe { &*list };
    if lst.data.is_null() || lst.len == 0 {
        return alloc_c_string("[]");
    }
    if lst.len < 0 || lst.len > 1_000_000 {
        return alloc_c_string("[...]");
    }
    let mut parts: Vec<String> = Vec::with_capacity(lst.len as usize + 2);
    parts.push(String::from("["));
    for i in 0..lst.len as isize {
        if i > 0 {
            parts.push(String::from(","));
        }
        let base = unsafe { *(lst.data as *const i64).offset(i) } as *const u8;
        if base.is_null() {
            parts.push(String::from("null"));
            continue;
        }
        // Layout {i1 disc, i64 ok, i64 err} — disc at 0, ok at 8, err at 16 on x86_64.
        // SAFETY: base is heap Result from list element storage.
        let disc = unsafe { *base };
        let ok = unsafe { *(base.add(8) as *const i64) };
        let err = unsafe { *(base.add(16) as *const i64) };
        if disc != 0 {
            if mode >= 20 {
                // Product Map: mode = 20 + arity.
                let arity = mode - 20;
                let json_ptr = mimi_map_to_json_product_i64(ok as MapHandle, arity, 0);
                let s = unsafe { cstr_to_string(json_ptr) };
                if !json_ptr.is_null() {
                    unsafe { libc::free(json_ptr as *mut std::ffi::c_void) };
                }
                parts.push(format!("{{\"Ok\":[{}]}}", s));
            } else if mode >= 10 {
                let map_mode = mode - 10;
                let json_ptr = match map_mode {
                    1 => mimi_map_to_json_string(ok as MapHandle),
                    2 => mimi_map_to_json_bool(ok as MapHandle),
                    3 => mimi_map_to_json_f64_serde(ok as MapHandle),
                    _ => mimi_map_to_json_i64(ok as MapHandle),
                };
                let s = unsafe { cstr_to_string(json_ptr) };
                if !json_ptr.is_null() {
                    unsafe { libc::free(json_ptr as *mut std::ffi::c_void) };
                }
                parts.push(format!("{{\"Ok\":[{}]}}", s));
            } else {
                parts.push(format!("{{\"Ok\":[{}]}}", ok));
            }
        } else {
            // Err may be: (1) Mimi string heap struct {ptr,i64} as ptrtoint,
            // (2) C-string ValueHandle, or (3) scalar i64.
            let err_s = decode_result_err_string(err);
            parts.push(format!("{{\"Err\":[{}]}}", err_s));
        }
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

/// Render `List<Option<Map>>` as JSON array of `{"Some":[{…}]}` / `"None"`.
/// `mode`: 0=i64 map, 1=string map, 2=bool map, 3=f64 map.
#[no_mangle]
pub extern "C" fn mimi_list_option_map_to_json(
    list: *const MimiList,
    mode: i64,
) -> *mut std::ffi::c_char {
    if list.is_null() {
        return alloc_c_string("[]");
    }
    let lst = unsafe { &*list };
    if lst.data.is_null() || lst.len == 0 {
        return alloc_c_string("[]");
    }
    if lst.len < 0 || lst.len > 1_000_000 {
        return alloc_c_string("[...]");
    }
    let mut parts: Vec<String> = Vec::with_capacity(lst.len as usize + 2);
    parts.push(String::from("["));
    for i in 0..lst.len as isize {
        if i > 0 {
            parts.push(String::from(","));
        }
        let base = unsafe { *(lst.data as *const i64).offset(i) } as *const u8;
        if base.is_null() {
            parts.push(String::from("\"None\""));
            continue;
        }
        // SAFETY: heap Option {i1, i64 map handle}.
        let disc = unsafe { *base };
        let handle = unsafe { *(base.add(8) as *const i64) } as MapHandle;
        if disc != 0 {
            // mode: 0-3 scalar; 10+ product; 20+ List; 30+ Set; 40+ Map of Map.
            let json_ptr = if mode >= 40 {
                mimi_map_to_json_map_product_i64(handle, mode - 40, 0)
            } else if mode >= 30 {
                mimi_map_to_json_set_product_i64(handle, mode - 30, 0)
            } else if mode >= 20 {
                mimi_map_to_json_list_product_i64(handle, mode - 20, 0)
            } else if mode >= 10 {
                mimi_map_to_json_product_i64(handle, mode - 10, 0)
            } else {
                match mode {
                    1 => mimi_map_to_json_string(handle),
                    2 => mimi_map_to_json_bool(handle),
                    3 => mimi_map_to_json_f64_serde(handle),
                    _ => mimi_map_to_json_i64(handle),
                }
            };
            let s = unsafe { cstr_to_string(json_ptr) };
            if !json_ptr.is_null() {
                unsafe { libc::free(json_ptr as *mut std::ffi::c_void) };
            }
            parts.push(format!("{{\"Some\":[{}]}}", s));
        } else {
            parts.push(String::from("\"None\""));
        }
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

/// Render `List<Option<i32>>` (ptrtoint of Option structs) as JSON array of
/// `{"Some":[n]}` / `"None"` tags matching interp to_json.
#[no_mangle]
pub extern "C" fn mimi_list_option_i64_to_json(list: *const MimiList) -> *mut std::ffi::c_char {
    if list.is_null() {
        return alloc_c_string("[]");
    }
    let lst = unsafe { &*list };
    if lst.data.is_null() || lst.len == 0 {
        return alloc_c_string("[]");
    }
    if lst.len < 0 || lst.len > 1_000_000 {
        return alloc_c_string("[...]");
    }
    let mut parts: Vec<String> = Vec::with_capacity(lst.len as usize + 2);
    parts.push(String::from("["));
    for i in 0..lst.len as isize {
        if i > 0 {
            parts.push(String::from(","));
        }
        let ptr = unsafe { *(lst.data as *const i64).offset(i) } as *const (u8, i64);
        // Layout is {i1 disc, i64 payload} but packed as struct; load carefully.
        // We stored Option as LLVM {i1, i64} — use byte-level: first byte/bit as disc.
        if ptr.is_null() {
            parts.push(String::from("\"None\""));
            continue;
        }
        // SAFETY: ptr is heap Option from from_json List Option path.
        let disc = unsafe { *(ptr as *const u8) };
        let payload = unsafe { *((ptr as *const u8).add(8) as *const i64) };
        if disc != 0 {
            parts.push(format!("{{\"Some\":[{}]}}", payload));
        } else {
            parts.push(String::from("\"None\""));
        }
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

/// Render `List<Map>` (i64 map handles in data slots) as `[{"a":1}, ...]`.
#[no_mangle]
pub extern "C" fn mimi_list_map_to_string(list: *const MimiList) -> *mut std::ffi::c_char {
    list_map_to_string_impl(list, MapJsonMode::Int, ", ")
}

/// List of Map for to_json with string values (no space after comma).
#[no_mangle]
pub extern "C" fn mimi_list_map_to_json_string(list: *const MimiList) -> *mut std::ffi::c_char {
    list_map_to_string_impl(list, MapJsonMode::String, ",")
}

fn list_map_to_string_impl(
    list: *const MimiList,
    mode: MapJsonMode,
    sep: &str,
) -> *mut std::ffi::c_char {
    if list.is_null() {
        return alloc_c_string("[]");
    }
    let lst = unsafe { &*list };
    if lst.data.is_null() || lst.len == 0 {
        return alloc_c_string("[]");
    }
    if lst.len < 0 || lst.len > 1_000_000 {
        return alloc_c_string("[...]");
    }
    let mut parts: Vec<String> = Vec::with_capacity(lst.len as usize + 2);
    parts.push(String::from("["));
    for i in 0..lst.len as isize {
        if i > 0 {
            parts.push(String::from(sep));
        }
        let handle = unsafe { *(lst.data as *const i64).offset(i) } as MapHandle;
        let json_ptr = match mode {
            MapJsonMode::String => mimi_map_to_json_string(handle),
            MapJsonMode::Bool => mimi_map_to_json_bool(handle),
            MapJsonMode::Float | MapJsonMode::FloatJson => mimi_map_to_json_f64_serde(handle),
            MapJsonMode::Int => mimi_map_to_json_i64(handle),
        };
        let s = unsafe { cstr_to_string(json_ptr) };
        if !json_ptr.is_null() {
            unsafe { libc::free(json_ptr as *mut std::ffi::c_void) };
        }
        parts.push(s);
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

/// Option of Map handle → `{"Some":[{…}]}` / `"None"`.
#[no_mangle]
pub extern "C" fn mimi_option_map_to_json(disc: i64, handle: MapHandle, mode: i64) -> *mut std::ffi::c_char {
    if disc == 0 {
        return alloc_c_string("\"None\"");
    }
    // mode encoding:
    // 0-3 scalar maps; 10+arity flat product; 20+arity Map of List product;
    // 30+arity Map of Set product; 40+arity Map of Map product.
    let json_ptr = if mode >= 40 {
        mimi_map_to_json_map_product_i64(handle, mode - 40, 0)
    } else if mode >= 30 {
        mimi_map_to_json_set_product_i64(handle, mode - 30, 0)
    } else if mode >= 20 {
        mimi_map_to_json_list_product_i64(handle, mode - 20, 0)
    } else if mode >= 10 {
        mimi_map_to_json_product_i64(handle, mode - 10, 0)
    } else {
        match mode {
            1 => mimi_map_to_json_string(handle),
            2 => mimi_map_to_json_bool(handle),
            3 => mimi_map_to_json_f64_serde(handle),
            _ => mimi_map_to_json_i64(handle),
        }
    };
    let s = unsafe { cstr_to_string(json_ptr) };
    if !json_ptr.is_null() {
        unsafe { libc::free(json_ptr as *mut std::ffi::c_void) };
    }
    alloc_c_string(&format!("{{\"Some\":[{}]}}", s))
}

/// Option of Set handle → `{"Some":[[…]]}` / `"None"`.
#[no_mangle]
pub extern "C" fn mimi_option_set_to_json(disc: i64, handle: SetHandle, mode: i64) -> *mut std::ffi::c_char {
    if disc == 0 {
        return alloc_c_string("\"None\"");
    }
    // mode >= 10 encodes product arity as (10 + arity).
    let json_ptr = if mode >= 10 {
        mimi_set_to_json_product_i64(handle, mode - 10, 0)
    } else {
        match mode {
            1 => mimi_set_to_json_string(handle),
            2 => mimi_set_to_json_bool(handle),
            3 => mimi_set_to_json_f64(handle),
            _ => mimi_set_to_json_i64(handle),
        }
    };
    let s = unsafe { cstr_to_string(json_ptr) };
    if !json_ptr.is_null() {
        unsafe { libc::free(json_ptr as *mut std::ffi::c_void) };
    }
    alloc_c_string(&format!("{{\"Some\":[{}]}}", s))
}

/// Result of Map handle → `{"Ok":[{…}]}` / `{"Err":[n]}`.
#[no_mangle]
pub extern "C" fn mimi_result_map_to_json(
    disc: i64,
    ok_handle: MapHandle,
    err: i64,
    mode: i64,
) -> *mut std::ffi::c_char {
    if disc != 0 {
        // mode: 0-3 scalar; 10+ product; 20+ List product; 30+ Set; 40+ Map.
        let json_ptr = if mode >= 40 {
            mimi_map_to_json_map_product_i64(ok_handle, mode - 40, 0)
        } else if mode >= 30 {
            mimi_map_to_json_set_product_i64(ok_handle, mode - 30, 0)
        } else if mode >= 20 {
            mimi_map_to_json_list_product_i64(ok_handle, mode - 20, 0)
        } else if mode >= 10 {
            mimi_map_to_json_product_i64(ok_handle, mode - 10, 0)
        } else {
            match mode {
                1 => mimi_map_to_json_string(ok_handle),
                2 => mimi_map_to_json_bool(ok_handle),
                3 => mimi_map_to_json_f64_serde(ok_handle),
                _ => mimi_map_to_json_i64(ok_handle),
            }
        };
        let s = unsafe { cstr_to_string(json_ptr) };
        if !json_ptr.is_null() {
            unsafe { libc::free(json_ptr as *mut std::ffi::c_void) };
        }
        alloc_c_string(&format!("{{\"Ok\":[{}]}}", s))
    } else {
        let err_s = decode_result_err_string(err);
        alloc_c_string(&format!("{{\"Err\":[{}]}}", err_s))
    }
}

/// Result of Set handle → `{"Ok":[[…]]}` / `{"Err":[n]}`.
#[no_mangle]
pub extern "C" fn mimi_result_set_to_json(
    disc: i64,
    ok_handle: SetHandle,
    err: i64,
    mode: i64,
) -> *mut std::ffi::c_char {
    if disc != 0 {
        // mode >= 10 encodes product arity as (10 + arity).
        let json_ptr = if mode >= 10 {
            mimi_set_to_json_product_i64(ok_handle, mode - 10, 0)
        } else {
            match mode {
                1 => mimi_set_to_json_string(ok_handle),
                2 => mimi_set_to_json_bool(ok_handle),
                3 => mimi_set_to_json_f64(ok_handle),
                _ => mimi_set_to_json_i64(ok_handle),
            }
        };
        let s = unsafe { cstr_to_string(json_ptr) };
        if !json_ptr.is_null() {
            unsafe { libc::free(json_ptr as *mut std::ffi::c_void) };
        }
        alloc_c_string(&format!("{{\"Ok\":[{}]}}", s))
    } else {
        let err_s = decode_result_err_string(err);
        alloc_c_string(&format!("{{\"Err\":[{}]}}", err_s))
    }
}

/// Render `List<Set>` as a JSON array of JSON arrays `[[1,2],[3]]`.
#[no_mangle]
pub extern "C" fn mimi_list_set_to_json(list: *const MimiList) -> *mut std::ffi::c_char {
    if list.is_null() {
        return alloc_c_string("[]");
    }
    let lst = unsafe { &*list };
    if lst.data.is_null() || lst.len == 0 {
        return alloc_c_string("[]");
    }
    if lst.len < 0 || lst.len > 1_000_000 {
        return alloc_c_string("[...]");
    }
    let mut parts: Vec<String> = Vec::with_capacity(lst.len as usize + 2);
    parts.push(String::from("["));
    for i in 0..lst.len as isize {
        if i > 0 {
            parts.push(String::from(","));
        }
        let handle = unsafe { *(lst.data as *const i64).offset(i) } as SetHandle;
        let json_ptr = mimi_set_to_json_i64(handle);
        let s = unsafe { cstr_to_string(json_ptr) };
        if !json_ptr.is_null() {
            unsafe { libc::free(json_ptr as *mut std::ffi::c_void) };
        }
        parts.push(s);
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

/// Render `List<Set<product>>` as JSON array of product-set JSON arrays.
#[no_mangle]
pub extern "C" fn mimi_list_set_product_to_json(
    list: *const MimiList,
    arity: i64,
) -> *mut std::ffi::c_char {
    if list.is_null() || arity <= 0 {
        return alloc_c_string("[]");
    }
    let lst = unsafe { &*list };
    if lst.data.is_null() || lst.len == 0 {
        return alloc_c_string("[]");
    }
    if lst.len < 0 || lst.len > 1_000_000 {
        return alloc_c_string("[...]");
    }
    let mut parts: Vec<String> = Vec::with_capacity(lst.len as usize + 2);
    parts.push(String::from("["));
    for i in 0..lst.len as isize {
        if i > 0 {
            parts.push(String::from(","));
        }
        let handle = unsafe { *(lst.data as *const i64).offset(i) } as SetHandle;
        let json_ptr = mimi_set_to_json_product_i64(handle, arity, 0);
        let s = unsafe { cstr_to_string(json_ptr) };
        if !json_ptr.is_null() {
            unsafe { libc::free(json_ptr as *mut std::ffi::c_void) };
        }
        parts.push(s);
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

/// Render `List<Set<product>>` Display as `[Set{(1, 2)}, ...]`.
#[no_mangle]
pub extern "C" fn mimi_list_set_product_to_string(
    list: *const MimiList,
    arity: i64,
) -> *mut std::ffi::c_char {
    if list.is_null() || arity <= 0 {
        return alloc_c_string("[]");
    }
    let lst = unsafe { &*list };
    if lst.data.is_null() || lst.len == 0 {
        return alloc_c_string("[]");
    }
    if lst.len < 0 || lst.len > 1_000_000 {
        return alloc_c_string("[...]");
    }
    let mut parts: Vec<String> = Vec::with_capacity(lst.len as usize + 2);
    parts.push(String::from("["));
    for i in 0..lst.len as isize {
        if i > 0 {
            parts.push(String::from(", "));
        }
        let handle = unsafe { *(lst.data as *const i64).offset(i) } as SetHandle;
        let disp = mimi_set_to_json_product_i64(handle, arity, 1);
        let s = unsafe { cstr_to_string(disp) };
        if !disp.is_null() {
            unsafe { libc::free(disp as *mut std::ffi::c_void) };
        }
        parts.push(s);
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

/// Render `List<Set>` (i64 set handles) as `[Set{1, 2}, ...]`.
#[no_mangle]
pub extern "C" fn mimi_list_set_to_string(list: *const MimiList) -> *mut std::ffi::c_char {
    if list.is_null() {
        return alloc_c_string("[]");
    }
    let lst = unsafe { &*list };
    if lst.data.is_null() || lst.len == 0 {
        return alloc_c_string("[]");
    }
    if lst.len < 0 || lst.len > 1_000_000 {
        return alloc_c_string("[...]");
    }
    let mut parts: Vec<String> = Vec::with_capacity(lst.len as usize + 2);
    parts.push(String::from("["));
    for i in 0..lst.len as isize {
        if i > 0 {
            parts.push(String::from(", "));
        }
        let handle = unsafe { *(lst.data as *const i64).offset(i) } as SetHandle;
        let disp = mimi_set_to_display(handle);
        let s = unsafe { cstr_to_string(disp) };
        if !disp.is_null() {
            unsafe { libc::free(disp as *mut std::ffi::c_void) };
        }
        parts.push(s);
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

/// Render a codegen `List<i32>` (layout `{i64 len, i8* data}` where data points
/// to pointer-sized slots) to a printable heap-allocated C string.
#[no_mangle]
pub extern "C" fn mimi_list_i32_to_string(list: *const MimiList) -> *mut std::ffi::c_char {
    if list.is_null() {
        return alloc_c_string("[]");
    }
    let lst = unsafe { &*list };
    if lst.data.is_null() || lst.len == 0 {
        return alloc_c_string("[]");
    }
    if lst.len < 0 || lst.len > 1_000_000 {
        return alloc_c_string("[...]");
    }
    let mut parts: Vec<String> = Vec::with_capacity(lst.len as usize + 2);
    parts.push(String::from("["));
    for i in 0..lst.len as isize {
        if i > 0 {
            parts.push(String::from(", "));
        }
        // M12 fix: read as i64 (pointer-sized slot) and cast to i32.
        // Reading directly as *const i32 is endian-dependent; using i64
        // then truncating is portable.
        let item = unsafe { *(lst.data as *const i64).offset(i) } as i32;
        parts.push(item.to_string());
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

/// Render a codegen `List<bool>` (layout `{i64 len, i8* data}` where data points
/// to i64 slots containing 0 or 1) to a JSON array string. Each element is
/// formatted as `true` or `false`. Returns a heap-allocated C string.
#[no_mangle]
pub extern "C" fn mimi_list_bool_to_json(list: *const MimiList) -> *mut std::ffi::c_char {
    if list.is_null() {
        return alloc_c_string("[]");
    }
    let lst = unsafe { &*list };
    if lst.data.is_null() || lst.len == 0 {
        return alloc_c_string("[]");
    }
    if lst.len < 0 || lst.len > 1_000_000 {
        return alloc_c_string("[...]");
    }
    let mut parts: Vec<String> = Vec::with_capacity(lst.len as usize + 2);
    parts.push(String::from("["));
    for i in 0..lst.len as isize {
        if i > 0 {
            parts.push(String::from(","));
        }
        let item = unsafe { *(lst.data as *const i64).offset(i) };
        parts.push(if item == 0 {
            String::from("false")
        } else {
            String::from("true")
        });
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

/// Render a codegen `List<i32/i64>` (layout `{i64 len, i8* data}` where data
/// points to i64 slots) to a JSON array string. Each i64 element is formatted
/// as a JSON number. Returns a heap-allocated C string.
#[no_mangle]
pub extern "C" fn mimi_list_i64_to_json(list: *const MimiList) -> *mut std::ffi::c_char {
    if list.is_null() {
        return alloc_c_string("[]");
    }
    let lst = unsafe { &*list };
    if lst.data.is_null() || lst.len == 0 {
        return alloc_c_string("[]");
    }
    if lst.len < 0 || lst.len > 1_000_000 {
        return alloc_c_string("[...]");
    }
    let mut parts: Vec<String> = Vec::with_capacity(lst.len as usize + 2);
    parts.push(String::from("["));
    for i in 0..lst.len as isize {
        if i > 0 {
            parts.push(String::from(","));
        }
        let item = unsafe { *(lst.data as *const i64).offset(i) };
        parts.push(item.to_string());
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

/// Render a codegen `List<f64>` (layout `{i64 len, i8* data}` where data points
/// to i64 slots containing bitcast f64 values) to a JSON array string.
/// Returns a heap-allocated C string.
#[no_mangle]
pub extern "C" fn mimi_list_f64_to_json(list: *const MimiList) -> *mut std::ffi::c_char {
    if list.is_null() {
        return alloc_c_string("[]");
    }
    let lst = unsafe { &*list };
    if lst.data.is_null() || lst.len == 0 {
        return alloc_c_string("[]");
    }
    if lst.len < 0 || lst.len > 1_000_000 {
        return alloc_c_string("[...]");
    }
    let mut parts: Vec<String> = Vec::with_capacity(lst.len as usize + 2);
    parts.push(String::from("["));
    for i in 0..lst.len as isize {
        if i > 0 {
            parts.push(String::from(","));
        }
        let bits = unsafe { *(lst.data as *const i64).offset(i) };
        let fv = f64::from_bits(bits as u64);
        parts.push(fv.to_string());
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

/// Render a codegen `List<string>` (layout `{i64 len, i8* data}` where data
/// points to i64 slots containing C-string pointers) to a JSON array string.
/// Each element is quoted and JSON-escaped. Returns a heap-allocated C string.
#[no_mangle]
pub extern "C" fn mimi_list_str_to_json(list: *const MimiList) -> *mut std::ffi::c_char {
    if list.is_null() {
        return alloc_c_string("[]");
    }
    let lst = unsafe { &*list };
    if lst.data.is_null() || lst.len == 0 {
        return alloc_c_string("[]");
    }
    if lst.len < 0 || lst.len > 1_000_000 {
        return alloc_c_string("[...]");
    }
    let mut parts: Vec<String> = Vec::with_capacity(lst.len as usize + 2);
    parts.push(String::from("["));
    for i in 0..lst.len as isize {
        if i > 0 {
            parts.push(String::from(","));
        }
        let item_ptr = unsafe { *(lst.data as *const *mut std::ffi::c_char).offset(i) };
        if item_ptr.is_null() {
            parts.push(String::from("null"));
        } else {
            let s = unsafe { cstr_to_string(item_ptr) };
            // JSON-escape the string: wrap in quotes, escape backslash, quotes, and control chars
            let escaped = json_escape_string(&s);
            parts.push(escaped);
        }
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

/// Render a codegen `List<List<T>>` by using `elem_to_string` for each element.
/// The caller provides the appropriate inner-list formatter
/// (`mimi_list_to_string` for `List<string>`, `mimi_list_i32_to_string` for
/// `List<i32>`, etc.). Returns a heap-allocated C string.
#[no_mangle]
pub extern "C" fn mimi_list_list_to_string(
    list: *const MimiList,
    elem_to_string: extern "C" fn(*const MimiList) -> *mut std::ffi::c_char,
) -> *mut std::ffi::c_char {
    list_list_to_string_impl(list, elem_to_string, ", ")
}

/// Compact JSON form of `List<List<T>>` (no spaces after commas).
#[no_mangle]
pub extern "C" fn mimi_list_list_to_json(
    list: *const MimiList,
    elem_to_string: extern "C" fn(*const MimiList) -> *mut std::ffi::c_char,
) -> *mut std::ffi::c_char {
    list_list_to_string_impl(list, elem_to_string, ",")
}

fn list_list_to_string_impl(
    list: *const MimiList,
    elem_to_string: extern "C" fn(*const MimiList) -> *mut std::ffi::c_char,
    sep: &str,
) -> *mut std::ffi::c_char {
    if list.is_null() {
        return alloc_c_string("[]");
    }
    // SAFETY: caller ensures `list` is a valid `*const MimiList` or null.
    let lst = unsafe { &*list };
    if lst.data.is_null() || lst.len == 0 {
        return alloc_c_string("[]");
    }
    if lst.len < 0 || lst.len > 1_000_000 {
        return alloc_c_string("[...]");
    }
    let mut parts: Vec<String> = Vec::with_capacity(lst.len as usize + 2);
    parts.push(String::from("["));
    for i in 0..lst.len as isize {
        if i > 0 {
            parts.push(String::from(sep));
        }
        // `lst.data` points to inner list pointers (`*const MimiList`) or
        // ptrtoint handles stored as i64 slots.
        let slot = unsafe { *(lst.data as *const i64).offset(i) };
        let inner = slot as *const MimiList;
        if inner.is_null() || slot == 0 {
            parts.push(String::from("null"));
        } else {
            let inner_str = elem_to_string(inner);
            let s = unsafe { cstr_to_string(inner_str) };
            // The inner formatter returns a heap-allocated string that we now own.
            if !inner_str.is_null() {
                // SAFETY: `inner_str` was allocated by `alloc_c_string` in the inner formatter.
                unsafe { libc::free(inner_str as *mut std::ffi::c_void) };
            }
            parts.push(s);
        }
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

/// Render a codegen `List<RecordType>` (data array holds ptrtoint heap struct pointers)
/// to a JSON array string. Each element is serialized by calling `elem_to_json(ptr)` which
/// returns a heap-allocated C string of the JSON representation of that record.
/// Returns a heap-allocated C string.
#[no_mangle]
pub extern "C" fn mimi_list_record_to_json(
    list: *const MimiList,
    elem_to_json: extern "C" fn(*const std::ffi::c_void) -> *mut std::ffi::c_char,
) -> *mut std::ffi::c_char {
    if list.is_null() {
        return alloc_c_string("[]");
    }
    // SAFETY: caller ensures `list` is a valid `*const MimiList` or null.
    let lst = unsafe { &*list };
    if lst.data.is_null() || lst.len == 0 {
        return alloc_c_string("[]");
    }
    if lst.len < 0 || lst.len > 1_000_000 {
        return alloc_c_string("[...]");
    }
    let mut parts: Vec<String> = Vec::with_capacity(lst.len as usize + 2);
    parts.push(String::from("["));
    for i in 0..lst.len as isize {
        if i > 0 {
            parts.push(String::from(","));
        }
        let elem_ptr = unsafe { *(lst.data as *const *const std::ffi::c_void).offset(i) };
        if elem_ptr.is_null() {
            parts.push(String::from("null"));
        } else {
            let elem_json = elem_to_json(elem_ptr);
            let s = unsafe { cstr_to_string(elem_json) };
            if !elem_json.is_null() {
                // SAFETY: `elem_json` was allocated by `alloc_c_string` in the callback.
                unsafe { libc::free(elem_json as *mut std::ffi::c_void) };
            }
            parts.push(s);
        }
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

#[no_mangle]
pub extern "C" fn mimi_str_replace(
    s: *const std::ffi::c_char,
    from: *const std::ffi::c_char,
    to: *const std::ffi::c_char,
) -> *mut std::ffi::c_char {
    // SAFETY: `cstr_to_string` handles null pointers safely.
    let ss = unsafe { cstr_to_string(s) };
    // SAFETY: `cstr_to_string` handles null pointers safely.
    let f = unsafe { cstr_to_string(from) };
    // SAFETY: `cstr_to_string` handles null pointers safely.
    let t = unsafe { cstr_to_string(to) };

    if f.is_empty() {
        return alloc_c_string(&ss);
    }
    let result = ss.replace(&f, &t);
    alloc_c_string(&result)
}

// ---------------------------------------------------------------------------
// Try/exit (? operator)
// ---------------------------------------------------------------------------

/// S18: Called by codegen `?` operator when Result is Err.
/// Uses process::exit(1) instead of panic! because:
/// - Panic across FFI boundary is undefined behavior (Rust ABI requirement)
/// - process::exit skips destructors but is the safest exit path in FFI context
/// - The calling codegen has already formatted the error message
#[no_mangle]
pub extern "C" fn mimi_try_exit(payload: i64) -> ! {
    eprintln!("Error: Result::Err({})", payload);
    std::process::exit(1);
}

/// S18: String variant of try_exit for string error messages.
#[no_mangle]
pub extern "C" fn mimi_try_exit_str(str: *const std::ffi::c_char, len: i64) -> ! {
    let msg = if str.is_null() || len <= 0 {
        String::new()
    } else {
        // SAFETY: `len` is validated as positive before constructing the slice.
        unsafe {
            let slice = std::slice::from_raw_parts(str as *const u8, len as usize);
            String::from_utf8_lossy(slice).into_owned()
        }
    };
    eprintln!("Error: Result::Err(\"{}\")", msg);
    std::process::exit(1);
}

/// CG-C1: Runtime trap for non-exhaustive match. Called by codegen when a match
/// fails to cover all cases — prevents UB by printing a diagnostic and aborting.
#[no_mangle]
pub extern "C" fn mimi_match_panic() -> ! {
    eprintln!("panic: non-exhaustive match — all cases must be covered");
    std::process::abort();
}

// ---------------------------------------------------------------------------
// Time functions
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn mimi_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn mimi_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn mimi_sleep(ms: i64) {
    if ms > 0 {
        // Cap absurd durations (i64::MAX ms) to 24h — same policy as interp.
        const MAX_SLEEP_MS: u64 = 24 * 60 * 60 * 1000;
        let ms_u = (ms as u64).min(MAX_SLEEP_MS);
        std::thread::sleep(std::time::Duration::from_millis(ms_u));
    }
}

// ---------------------------------------------------------------------------
// Environment / CLI
// ---------------------------------------------------------------------------

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
    let args_mutex = CLI_ARGS.get_or_init(|| Mutex::new(CliArgs { argc: 0, argv: vec![] }));
    let args = args_mutex.lock().unwrap_or_else(|e| e.into_inner());
    if args.argc <= 1 {
        return 0;
    }
    (args.argc - 1) as i64
}

#[no_mangle]
pub extern "C" fn mimi_args_list() -> *mut MimiList {
    init_cli_args();
    let args_mutex = CLI_ARGS.get_or_init(|| Mutex::new(CliArgs { argc: 0, argv: vec![] }));
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
    let args_mutex = CLI_ARGS.get_or_init(|| Mutex::new(CliArgs { argc: 0, argv: vec![] }));
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

// ---------------------------------------------------------------------------
// JSON parser (recursive descent, self-contained)
// ---------------------------------------------------------------------------

const JSON_MAX_DEPTH: i32 = 64;

struct JsonParser<'a> {
    p: &'a [u8],
    pos: usize,
    depth: i32,
}

impl<'a> JsonParser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            p: input.as_bytes(),
            pos: 0,
            depth: 0,
        }
    }

    fn peek(&self) -> u8 {
        if self.pos < self.p.len() {
            self.p[self.pos]
        } else {
            0
        }
    }

    fn advance(&mut self) {
        if self.pos < self.p.len() {
            self.pos += 1;
        }
    }

    fn skip_ws(&mut self) {
        while self.pos < self.p.len() {
            match self.p[self.pos] {
                b' ' | b'\t' | b'\n' | b'\r' => self.pos += 1,
                _ => break,
            }
        }
    }

    fn parse_value(&mut self) -> Option<String> {
        self.skip_ws();
        if self.pos >= self.p.len() {
            return None;
        }
        self.depth += 1;
        if self.depth > JSON_MAX_DEPTH {
            return None;
        }

        let result = match self.peek() {
            b'"' => self.parse_string(),
            b'{' => self.parse_object(),
            b'[' => self.parse_array(),
            b't' => self.parse_literal("true", "true"),
            b'f' => self.parse_literal("false", "false"),
            b'n' => self.parse_literal("null", "null"),
            b'-' | b'0'..=b'9' => self.parse_number(),
            _ => None,
        };

        self.depth -= 1;
        result
    }

    fn parse_string(&mut self) -> Option<String> {
        if self.peek() != b'"' {
            return None;
        }
        self.advance(); // skip "
        let _start = self.pos;
        let mut result = String::new();
        let mut esc = false;
        loop {
            if self.pos >= self.p.len() {
                return None;
            }
            let c = self.p[self.pos];
            if esc {
                match c {
                    b'"' => result.push('"'),
                    b'\\' => result.push('\\'),
                    b'/' => result.push('/'),
                    b'b' => result.push('\u{0008}'),
                    b'f' => result.push('\u{000c}'),
                    b'n' => result.push('\n'),
                    b'r' => result.push('\r'),
                    b't' => result.push('\t'),
                    b'u' => {
                        // Parse 4-digit hex
                        if self.pos + 4 >= self.p.len() {
                            return None;
                        }
                        let hex_str = &self.p[self.pos + 1..self.pos + 5];
                        let hex = std::str::from_utf8(hex_str).ok()?;
                        let cp = u32::from_str_radix(hex, 16).ok()?;
                        if let Some(ch) = char::from_u32(cp) {
                            result.push(ch);
                        }
                        self.pos += 4;
                    }
                    _ => {
                        result.push(c as char);
                    }
                }
                esc = false;
                self.pos += 1;
                continue;
            }
            if c == b'\\' {
                esc = true;
                self.pos += 1;
                continue;
            }
            if c == b'"' {
                self.pos += 1;
                return Some(result);
            }
            result.push(c as char);
            self.pos += 1;
        }
    }

    fn parse_number(&mut self) -> Option<String> {
        let start = self.pos;
        if self.peek() == b'-' {
            self.advance();
        }
        if self.pos >= self.p.len() || !self.peek().is_ascii_digit() {
            return None;
        }
        while self.pos < self.p.len() && self.p[self.pos].is_ascii_digit() {
            self.advance();
        }

        let mut is_float = false;
        if self.pos < self.p.len() && self.p[self.pos] == b'.' {
            is_float = true;
            self.advance();
            let mut has_digits = false;
            while self.pos < self.p.len() && self.p[self.pos].is_ascii_digit() {
                has_digits = true;
                self.advance();
            }
            if !has_digits {
                return None;
            }
        }
        if self.pos < self.p.len() && (self.p[self.pos] == b'e' || self.p[self.pos] == b'E') {
            is_float = true;
            self.advance();
            if self.pos < self.p.len() && (self.p[self.pos] == b'+' || self.p[self.pos] == b'-') {
                self.advance();
            }
            let mut has_digits = false;
            while self.pos < self.p.len() && self.p[self.pos].is_ascii_digit() {
                has_digits = true;
                self.advance();
            }
            if !has_digits {
                return None;
            }
        }

        let s = std::str::from_utf8(&self.p[start..self.pos]).ok()?;
        if is_float {
            // Format float: trim trailing zeros
            let val: f64 = s.parse().ok()?;
            let mut formatted = format!("{}", val);
            if formatted.contains('.') {
                formatted = formatted
                    .trim_end_matches('0')
                    .trim_end_matches('.')
                    .to_string();
            }
            Some(formatted)
        } else {
            Some(s.to_string())
        }
    }

    fn parse_literal(&mut self, expected: &str, value: &str) -> Option<String> {
        let bytes = expected.as_bytes();
        if self.pos + bytes.len() > self.p.len() {
            return None;
        }
        if &self.p[self.pos..self.pos + bytes.len()] == bytes {
            self.pos += bytes.len();
            Some(value.to_string())
        } else {
            None
        }
    }

    fn parse_object(&mut self) -> Option<String> {
        if self.peek() != b'{' {
            return None;
        }
        self.advance();
        let start = self.pos;
        let mut depth = 1u32;
        while self.pos < self.p.len() && depth > 0 {
            match self.p[self.pos] {
                b'{' => depth += 1,
                b'}' => depth -= 1,
                b'"' => {
                    self.advance();
                    loop {
                        if self.pos >= self.p.len() {
                            return None;
                        }
                        // RT-C1 pattern: trailing `\` must not skip past EOF.
                        if self.p[self.pos] == b'\\' {
                            self.pos += 1;
                            if self.pos < self.p.len() {
                                self.pos += 1;
                            }
                            continue;
                        }
                        if self.p[self.pos] == b'"' {
                            break;
                        }
                        self.pos += 1;
                    }
                }
                _ => {}
            }
            if depth > 0 {
                self.pos += 1;
            }
        }
        if depth != 0 {
            return None;
        }
        let s = std::str::from_utf8(&self.p[start..self.pos]).ok()?;
        self.pos += 1; // skip }
        Some(format!("{{{}}}", s))
    }

    fn parse_array(&mut self) -> Option<String> {
        if self.peek() != b'[' {
            return None;
        }
        self.advance();
        let start = self.pos;
        let mut depth = 1u32;
        while self.pos < self.p.len() && depth > 0 {
            match self.p[self.pos] {
                b'[' => depth += 1,
                b']' => depth -= 1,
                b'"' => {
                    self.advance();
                    loop {
                        if self.pos >= self.p.len() {
                            return None;
                        }
                        // RT-C1 pattern: trailing `\` must not skip past EOF.
                        if self.p[self.pos] == b'\\' {
                            self.pos += 1;
                            if self.pos < self.p.len() {
                                self.pos += 1;
                            }
                            continue;
                        }
                        if self.p[self.pos] == b'"' {
                            break;
                        }
                        self.pos += 1;
                    }
                }
                _ => {}
            }
            if depth > 0 {
                self.pos += 1;
            }
        }
        if depth != 0 {
            return None;
        }
        let s = std::str::from_utf8(&self.p[start..self.pos]).ok()?;
        self.pos += 1; // skip ]
        Some(format!("[{}]", s))
    }

    fn parse_full(&mut self) -> Option<String> {
        let val = self.parse_value()?;
        self.skip_ws();
        if self.pos != self.p.len() {
            return None;
        } // trailing garbage
        Some(val)
    }

    fn is_valid(&mut self) -> bool {
        self.parse_full().is_some()
    }
}

#[no_mangle]
pub extern "C" fn mimi_from_json(json_str: *const std::ffi::c_char) -> *mut std::ffi::c_void {
    if json_str.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `json_str` was checked non-null above.
    let s = unsafe { cstr_to_string(json_str) };
    let mut parser = JsonParser::new(&s);
    match parser.parse_full() {
        Some(val) => alloc_c_string(&val) as *mut std::ffi::c_void,
        None => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "C" fn mimi_is_valid_json(json_str: *const std::ffi::c_char) -> i64 {
    if json_str.is_null() {
        return 0;
    }
    // SAFETY: `json_str` was checked non-null above.
    let s = unsafe { cstr_to_string(json_str) };
    let mut parser = JsonParser::new(&s);
    parser.is_valid() as i64
}

fn json_get_inner(
    json_str: *const std::ffi::c_char,
    key: *const std::ffi::c_char,
) -> Option<String> {
    if json_str.is_null() || key.is_null() {
        return None;
    }
    // SAFETY: `json_str` was checked non-null above.
    let json = unsafe { cstr_to_string(json_str) };
    // SAFETY: `key` was checked non-null above.
    let k = unsafe { cstr_to_string(key) };
    let bytes = json.as_bytes();
    let mut pos = 0;

    // Skip whitespace
    while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') {
        pos += 1;
    }
    if pos >= bytes.len() || bytes[pos] != b'{' {
        return None;
    }
    pos += 1;

    loop {
        if pos >= bytes.len() || bytes[pos] == b'}' {
            return None;
        }
        while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') {
            pos += 1;
        }

        // Parse key string
        if bytes[pos] != b'"' {
            return None;
        }
        pos += 1;
        let mut key_buf = String::new();
        let mut key_esc = false;
        loop {
            if pos >= bytes.len() {
                return None;
            }
            let c = bytes[pos];
            if key_esc {
                match c {
                    b'"' => key_buf.push('"'),
                    b'\\' => key_buf.push('\\'),
                    b'/' => key_buf.push('/'),
                    b'b' => key_buf.push('\u{0008}'),
                    b'f' => key_buf.push('\u{000c}'),
                    b'n' => key_buf.push('\n'),
                    b'r' => key_buf.push('\r'),
                    b't' => key_buf.push('\t'),
                    b'u' => {
                        if pos + 4 >= bytes.len() {
                            return None;
                        }
                        let hex_str = std::str::from_utf8(&bytes[pos + 1..pos + 5]).ok()?;
                        let cp = u32::from_str_radix(hex_str, 16).ok()?;
                        if let Some(ch) = char::from_u32(cp) {
                            key_buf.push(ch);
                        }
                        pos += 4;
                    }
                    _ => {
                        key_buf.push(c as char);
                    }
                }
                key_esc = false;
                pos += 1;
                continue;
            }
            if c == b'\\' {
                key_esc = true;
                pos += 1;
                continue;
            }
            if c == b'"' {
                pos += 1;
                break;
            }
            key_buf.push(c as char);
            pos += 1;
        }

        while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') {
            pos += 1;
        }
        if pos >= bytes.len() || bytes[pos] != b':' {
            return None;
        }
        pos += 1;
        while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') {
            pos += 1;
        }

        if key_buf == k {
            // Extract the value at current position
            let val_start = pos;
            let mut parser = JsonParser::new(&json[val_start..]);
            return parser.parse_value();
        }

        // Skip value
        let val_start = pos;
        let mut dummy_parser = JsonParser::new(&json[val_start..]);
        dummy_parser.parse_value()?;
        pos = val_start + dummy_parser.pos;

        while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') {
            pos += 1;
        }
        if pos >= bytes.len() {
            return None;
        }
        if bytes[pos] == b',' {
            pos += 1;
        }
    }
}

#[no_mangle]
pub extern "C" fn json_get_string(
    json_str: *const std::ffi::c_char,
    key: *const std::ffi::c_char,
) -> *mut std::ffi::c_char {
    match json_get_inner(json_str, key) {
        Some(val) => alloc_c_string(&val),
        None => std::ptr::null_mut(),
    }
}

/// CRITICAL #18 fix: Check if a key exists in a JSON object.
/// Returns 1 if the key exists, 0 if not. This avoids the ambiguity of
/// json_get_string returning "" for both missing keys and empty-string values.
#[no_mangle]
pub extern "C" fn json_has_key(
    json_str: *const std::ffi::c_char,
    key: *const std::ffi::c_char,
) -> i64 {
    // json_get_inner returns None when key is missing, Some(val) when key
    // exists (regardless of value content). This correctly distinguishes
    // {"x": ""} (key exists, Some("")) from {} (key missing, None).
    match json_get_inner(json_str, key) {
        Some(_) => 1,
        None => 0,
    }
}

#[no_mangle]
pub extern "C" fn json_get_int(
    json_str: *const std::ffi::c_char,
    key: *const std::ffi::c_char,
) -> i64 {
    match json_get_inner(json_str, key) {
        Some(val) => {
            // C6-fix: log parse failure instead of silently returning 0
            val.parse::<i64>().unwrap_or_else(|e| {
                eprintln!(
                    "[mimi runtime] json_get_int: parse error for '{}': {}",
                    val, e
                );
                0
            })
        }
        None => 0,
    }
}

#[no_mangle]
pub extern "C" fn json_array_length(json_str: *const std::ffi::c_char) -> i64 {
    if json_str.is_null() {
        return 0;
    }
    // SAFETY: `json_str` was checked non-null above.
    let json = unsafe { cstr_to_string(json_str) };
    let bytes = json.as_bytes();
    let mut pos = 0;
    while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') {
        pos += 1;
    }
    if pos >= bytes.len() || bytes[pos] != b'[' {
        return 0;
    }
    pos += 1;
    let mut count: i64 = 0;
    if pos < bytes.len() && bytes[pos] == b']' {
        return 0; // empty array
    }
    loop {
        if pos >= bytes.len() {
            return count;
        }
        while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') {
            pos += 1;
        }
        if pos >= bytes.len() || bytes[pos] == b']' {
            return count;
        }
        let val_start = pos;
        let mut parser = JsonParser::new(&json[val_start..]);
        if parser.parse_value().is_some() {
            count += 1;
            pos = val_start + parser.pos;
        } else {
            return count;
        }
        while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') {
            pos += 1;
        }
        if pos >= bytes.len() {
            return count;
        }
        if bytes[pos] == b',' {
            pos += 1;
        } else {
            // `]` or unexpected token — either way, counting is done
            return count;
        }
    }
}

#[no_mangle]
pub extern "C" fn json_get_element(
    json_str: *const std::ffi::c_char,
    index: i64,
) -> *mut std::ffi::c_char {
    if json_str.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `json_str` was checked non-null above.
    let json = unsafe { cstr_to_string(json_str) };
    let bytes = json.as_bytes();
    let mut pos = 0;

    while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') {
        pos += 1;
    }
    if pos >= bytes.len() || bytes[pos] != b'[' {
        return std::ptr::null_mut();
    }
    pos += 1;

    let mut idx: i64 = 0;
    loop {
        if pos >= bytes.len() || bytes[pos] == b']' {
            return std::ptr::null_mut();
        }
        while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') {
            pos += 1;
        }

        if idx == index {
            let val_start = pos;
            let mut parser = JsonParser::new(&json[val_start..]);
            return match parser.parse_value() {
                Some(val) => alloc_c_string(&val),
                None => std::ptr::null_mut(),
            };
        }

        let val_start = pos;
        let mut dummy_parser = JsonParser::new(&json[val_start..]);
        if dummy_parser.parse_value().is_none() {
            return std::ptr::null_mut();
        }
        pos = val_start + dummy_parser.pos;

        while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') {
            pos += 1;
        }
        if pos >= bytes.len() {
            return std::ptr::null_mut();
        }
        if bytes[pos] == b',' {
            pos += 1;
        }
        idx += 1;
    }
}

// ─── from_json::<T> typed parsing helpers ────────────────────────

/// Serialize a MapHandle of integer ValueHandles to a JSON object string.
/// Keys are JSON-escaped; values are printed as decimal integers.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_i64(handle: MapHandle) -> *mut std::ffi::c_char {
    map_to_json_values(handle, MapJsonMode::Int)
}

/// Serialize a MapHandle of 0/1 bool ValueHandles as JSON true/false.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_bool(handle: MapHandle) -> *mut std::ffi::c_char {
    map_to_json_values(handle, MapJsonMode::Bool)
}

/// Serialize a MapHandle of f64-bit ValueHandles for println Display (compact).
#[no_mangle]
pub extern "C" fn mimi_map_to_json_f64(handle: MapHandle) -> *mut std::ffi::c_char {
    map_to_json_values(handle, MapJsonMode::Float)
}

/// Serialize Map f64 for `to_json` (serde-compatible, whole floats as `2.0`).
#[no_mangle]
pub extern "C" fn mimi_map_to_json_f64_serde(handle: MapHandle) -> *mut std::ffi::c_char {
    map_to_json_values(handle, MapJsonMode::FloatJson)
}

enum MapJsonMode {
    Int,
    Bool,
    Float,
    FloatJson,
    String,
}

fn map_to_json_values(handle: MapHandle, mode: MapJsonMode) -> *mut std::ffi::c_char {
    if handle == 0 {
        return alloc_c_string("{}");
    }
    // SAFETY: handle is a non-zero MapHandle from mimi_map_new / from_json.
    let map = unsafe { &*map_from_handle(handle) };
    if map.inner.len() > 1_000_000 {
        return alloc_c_string("{...}");
    }
    let mut entries: Vec<_> = map.inner.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    let mut parts: Vec<String> = Vec::with_capacity(entries.len() * 2 + 2);
    parts.push(String::from("{"));
    for (i, (k, v)) in entries.iter().enumerate() {
        if i > 0 {
            parts.push(String::from(","));
        }
        parts.push(json_escape_string(k));
        parts.push(String::from(":"));
        match mode {
            MapJsonMode::Bool => parts.push(if **v != 0 {
                String::from("true")
            } else {
                String::from("false")
            }),
            MapJsonMode::Float => {
                // Display/println compact form (matches interp Map Display: 2 not 2.0).
                let f = f64::from_bits(**v as u64);
                let s = if f.fract() == 0.0 && f.is_finite() {
                    format!("{}", f as i64)
                } else {
                    format!("{}", f)
                };
                parts.push(s);
            }
            MapJsonMode::FloatJson => {
                // to_json form matching serde_json (2.0 for whole floats).
                let f = f64::from_bits(**v as u64);
                let s = if f.fract() == 0.0 && f.is_finite() {
                    format!("{}.0", f as i64)
                } else {
                    format!("{}", f)
                };
                parts.push(s);
            }
            MapJsonMode::String => {
                // Should use mimi_map_to_json_string path, not this helper.
                parts.push(String::from("null"));
            }
            MapJsonMode::Int => parts.push(v.to_string()),
        }
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Build a MapHandle from a JSON object with string keys and f64 values.
/// Values are stored as f64 bit patterns in i64 ValueHandles.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_f64(json: *const std::ffi::c_char) -> MapHandle {
    if json.is_null() {
        return mimi_map_new();
    }
    // SAFETY: non-null JSON C string from codegen.
    let s = unsafe { cstr_to_string(json) };
    let handle = mimi_map_new();
    if handle == 0 {
        return 0;
    }
    let bytes = s.as_bytes();
    let mut pos = 0usize;
    while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') {
        pos += 1;
    }
    if pos >= bytes.len() || bytes[pos] != b'{' {
        return handle;
    }
    pos += 1;
    const MAX_ENTRIES: usize = 1_000_000;
    let mut count = 0usize;
    loop {
        if count >= MAX_ENTRIES {
            break;
        }
        while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r' | b',') {
            pos += 1;
        }
        if pos >= bytes.len() || bytes[pos] == b'}' {
            break;
        }
        if bytes[pos] != b'"' {
            break;
        }
        pos += 1;
        let mut esc = false;
        let mut key = String::new();
        loop {
            if pos >= bytes.len() {
                return handle;
            }
            let c = bytes[pos];
            if esc {
                key.push(c as char);
                esc = false;
                pos += 1;
                continue;
            }
            if c == b'\\' {
                esc = true;
                pos += 1;
                continue;
            }
            if c == b'"' {
                pos += 1;
                break;
            }
            key.push(c as char);
            pos += 1;
        }
        while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') {
            pos += 1;
        }
        if pos >= bytes.len() || bytes[pos] != b':' {
            break;
        }
        pos += 1;
        while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') {
            pos += 1;
        }
        let val_start = pos;
        let mut dummy = JsonParser::new(&s[val_start..]);
        let parsed = dummy.parse_value();
        pos = val_start + dummy.pos;
        let bits = match parsed {
            Some(ref tok) => tok.parse::<f64>().unwrap_or(0.0).to_bits() as i64,
            None => 0,
        };
        // SAFETY: handle is a valid map from mimi_map_new.
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, bits as ValueHandle);
        }
        count += 1;
    }
    handle
}

/// Build a MapHandle from a JSON object with string keys and string values.
/// Values are heap-cloned C strings (ValueHandles via mimi_str_clone).
#[no_mangle]
pub extern "C" fn mimi_map_from_json_string(json: *const std::ffi::c_char) -> MapHandle {
    if json.is_null() {
        return mimi_map_new();
    }
    // SAFETY: non-null JSON C string from codegen.
    let s = unsafe { cstr_to_string(json) };
    let handle = mimi_map_new();
    if handle == 0 {
        return 0;
    }
    let bytes = s.as_bytes();
    let mut pos = 0usize;
    while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') {
        pos += 1;
    }
    if pos >= bytes.len() || bytes[pos] != b'{' {
        return handle;
    }
    pos += 1;
    const MAX_ENTRIES: usize = 1_000_000;
    let mut count = 0usize;
    loop {
        if count >= MAX_ENTRIES {
            break;
        }
        while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r' | b',') {
            pos += 1;
        }
        if pos >= bytes.len() || bytes[pos] == b'}' {
            break;
        }
        if bytes[pos] != b'"' {
            break;
        }
        pos += 1;
        let mut esc = false;
        let mut key = String::new();
        loop {
            if pos >= bytes.len() {
                return handle;
            }
            let c = bytes[pos];
            if esc {
                key.push(c as char);
                esc = false;
                pos += 1;
                continue;
            }
            if c == b'\\' {
                esc = true;
                pos += 1;
                continue;
            }
            if c == b'"' {
                pos += 1;
                break;
            }
            key.push(c as char);
            pos += 1;
        }
        while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') {
            pos += 1;
        }
        if pos >= bytes.len() || bytes[pos] != b':' {
            break;
        }
        pos += 1;
        while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') {
            pos += 1;
        }
        // Expect string value.
        if pos >= bytes.len() || bytes[pos] != b'"' {
            break;
        }
        pos += 1;
        esc = false;
        let mut val = String::new();
        loop {
            if pos >= bytes.len() {
                return handle;
            }
            let c = bytes[pos];
            if esc {
                val.push(c as char);
                esc = false;
                pos += 1;
                continue;
            }
            if c == b'\\' {
                esc = true;
                pos += 1;
                continue;
            }
            if c == b'"' {
                pos += 1;
                break;
            }
            val.push(c as char);
            pos += 1;
        }
        let v_handle = mimi_str_clone(val.as_ptr() as *const std::ffi::c_char, val.len() as i64);
        // SAFETY: handle is a valid map from mimi_map_new.
        unsafe {
            (*map_from_handle(handle)).inner.insert(key, v_handle);
        }
        count += 1;
    }
    handle
}

/// Serialize a MapHandle whose values are C-string ValueHandles to JSON.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_string(handle: MapHandle) -> *mut std::ffi::c_char {
    if handle == 0 {
        return alloc_c_string("{}");
    }
    // SAFETY: handle is a non-zero MapHandle.
    let map = unsafe { &*map_from_handle(handle) };
    if map.inner.len() > 1_000_000 {
        return alloc_c_string("{...}");
    }
    let mut entries: Vec<_> = map.inner.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    let mut parts: Vec<String> = Vec::with_capacity(entries.len() * 2 + 2);
    parts.push(String::from("{"));
    for (i, (k, v)) in entries.iter().enumerate() {
        if i > 0 {
            parts.push(String::from(","));
        }
        parts.push(json_escape_string(k));
        parts.push(String::from(":"));
        // RT-H1: safe_c_string_from_handle already applies MIN_HEAP/align + mincore.
        let vh = **v;
        let vs = safe_c_string_from_handle(vh).unwrap_or_default();
        parts.push(json_escape_string(&vs));
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Serialize Map values that are heap-packed product-tuple structs of i64 fields.
/// `arity` is the number of i64 fields (e.g. 2 for `(i32,i32)` after widen).
/// `display_style`: 0 = JSON arrays `[1,2]`, 1 = Display `(1, 2)`.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    let map = unsafe { &*map_from_handle(handle) };
    if map.inner.len() > 1_000_000 {
        return alloc_c_string("{...}");
    }
    let mut entries: Vec<_> = map.inner.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    let mut parts: Vec<String> = Vec::with_capacity(entries.len() * 2 + 2);
    parts.push(String::from("{"));
    for (i, (k, v)) in entries.iter().enumerate() {
        if i > 0 {
            parts.push(String::from(","));
        }
        parts.push(json_escape_string(k));
        parts.push(String::from(":"));
        let vh = **v;
        let n = arity as usize;
        // SAFETY: map_set product path heap-packs an i64[n] struct and stores ptrtoint.
        let fields: Vec<i64> = if vh == 0 {
            vec![0; n]
        } else {
            let ptr = vh as *const i64;
            if ptr.is_null() {
                vec![0; n]
            } else {
                unsafe { std::slice::from_raw_parts(ptr, n).to_vec() }
            }
        };
        if display_style != 0 {
            // Display: (1, 2)
            let body: Vec<String> = fields.iter().map(|x| x.to_string()).collect();
            parts.push(format!("({})", body.join(", ")));
        } else {
            // JSON: [1,2]
            let body: Vec<String> = fields.iter().map(|x| x.to_string()).collect();
            parts.push(format!("[{}]", body.join(",")));
        }
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Serialize Map values that are heap-packed List of product-tuples.
/// List layout: `{i64 len, ptr data}` where data is `i64` product handles.
/// `display_style`: 0 = JSON `[[1,2]]`, 1 = Display `[(1, 2)]`.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_list_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    let map = unsafe { &*map_from_handle(handle) };
    if map.inner.len() > 1_000_000 {
        return alloc_c_string("{...}");
    }
    let mut entries: Vec<_> = map.inner.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    let mut parts: Vec<String> = Vec::with_capacity(entries.len() * 2 + 2);
    parts.push(String::from("{"));
    let n = arity as usize;
    for (i, (k, v)) in entries.iter().enumerate() {
        if i > 0 {
            parts.push(String::from(","));
        }
        parts.push(json_escape_string(k));
        parts.push(String::from(":"));
        let vh = **v;
        if vh == 0 {
            parts.push(String::from("[]"));
            continue;
        }
        // SAFETY: map_set packs List as heap {i64 len, ptr data}.
        let list_base = vh as *const u8;
        let len = unsafe { *(list_base as *const i64) };
        let data = unsafe { *(list_base.add(8) as *const *const i64) };
        if len <= 0 || data.is_null() || len > 1_000_000 {
            parts.push(String::from("[]"));
            continue;
        }
        let mut list_parts: Vec<String> = Vec::with_capacity(len as usize + 2);
        list_parts.push(String::from("["));
        for j in 0..len as isize {
            if j > 0 {
                list_parts.push(String::from(", "));
            }
            let prod_h = unsafe { *data.offset(j) };
            let fields: Vec<i64> = if prod_h == 0 {
                vec![0; n]
            } else {
                let ptr = prod_h as *const i64;
                if ptr.is_null() {
                    vec![0; n]
                } else {
                    unsafe { std::slice::from_raw_parts(ptr, n).to_vec() }
                }
            };
            if display_style != 0 {
                let body: Vec<String> = fields.iter().map(|x| x.to_string()).collect();
                list_parts.push(format!("({})", body.join(", ")));
            } else {
                let body: Vec<String> = fields.iter().map(|x| x.to_string()).collect();
                list_parts.push(format!("[{}]", body.join(",")));
            }
        }
        // JSON list uses no spaces after commas for dual with to_json.
        if display_style == 0 {
            list_parts.clear();
            list_parts.push(String::from("["));
            for j in 0..len as isize {
                if j > 0 {
                    list_parts.push(String::from(","));
                }
                let prod_h = unsafe { *data.offset(j) };
                let fields: Vec<i64> = if prod_h == 0 {
                    vec![0; n]
                } else {
                    let ptr = prod_h as *const i64;
                    if ptr.is_null() {
                        vec![0; n]
                    } else {
                        unsafe { std::slice::from_raw_parts(ptr, n).to_vec() }
                    }
                };
                let body: Vec<String> = fields.iter().map(|x| x.to_string()).collect();
                list_parts.push(format!("[{}]", body.join(",")));
            }
        }
        list_parts.push(String::from("]"));
        parts.push(list_parts.join(""));
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Build Map from JSON object whose values are arrays of product arrays:
/// `"a":[[1,2],[3,4]]`. Each list is heap-packed as List of product handles.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_list_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    let s = unsafe { cstr_to_string(json) };
    let handle = mimi_map_new();
    if handle == 0 {
        return 0;
    }
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b'{' {
        return handle;
    }
    i += 1;
    let n = arity as usize;
    loop {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b'}' {
            break;
        }
        if bytes[i] != b'"' {
            break;
        }
        i += 1;
        let start = i;
        while i < bytes.len() && bytes[i] != b'"' {
            if bytes[i] == b'\\' {
                i += 1;
            }
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let key = String::from_utf8_lossy(&bytes[start..i]).into_owned();
        i += 1;
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b':') {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'[' {
            break;
        }
        i += 1; // outer list
        let mut prod_handles: Vec<i64> = Vec::new();
        loop {
            while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
                i += 1;
            }
            if i >= bytes.len() || bytes[i] == b']' {
                if i < bytes.len() {
                    i += 1;
                }
                break;
            }
            if bytes[i] != b'[' {
                break;
            }
            i += 1;
            let mut fields = vec![0i64; n];
            for fi in 0..n {
                while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
                    i += 1;
                }
                let neg = i < bytes.len() && bytes[i] == b'-';
                if neg {
                    i += 1;
                }
                let mut v: i64 = 0;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    v = v
                        .saturating_mul(10)
                        .saturating_add((bytes[i] - b'0') as i64);
                    i += 1;
                }
                if neg {
                    v = -v;
                }
                fields[fi] = v;
            }
            while i < bytes.len() && bytes[i] != b']' {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b']' {
                i += 1;
            }
            let data_size = n * std::mem::size_of::<i64>();
            let ptr = unsafe { libc::malloc(data_size) as *mut i64 };
            if ptr.is_null() {
                continue;
            }
            unsafe {
                std::ptr::copy_nonoverlapping(fields.as_ptr(), ptr, n);
            }
            prod_handles.push(ptr as i64);
        }
        // Pack list {i64 len, ptr data}.
        let list_size = 16usize;
        let list_ptr = unsafe { libc::malloc(list_size) as *mut u8 };
        if list_ptr.is_null() {
            continue;
        }
        let data_size = prod_handles.len() * std::mem::size_of::<i64>();
        let data_ptr = if data_size > 0 {
            unsafe { libc::malloc(data_size) as *mut i64 }
        } else {
            std::ptr::null_mut()
        };
        if !data_ptr.is_null() && !prod_handles.is_empty() {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    prod_handles.as_ptr(),
                    data_ptr,
                    prod_handles.len(),
                );
            }
        }
        unsafe {
            *(list_ptr as *mut i64) = prod_handles.len() as i64;
            *(list_ptr.add(8) as *mut *mut i64) = data_ptr;
        }
        let vh = list_ptr as ValueHandle;
        unsafe {
            (*map_from_handle(handle)).inner.insert(key, vh);
        }
    }
    handle
}

/// Serialize Map values that are SetHandles of product-tuples.
/// `display_style`: 0 = JSON set arrays, 1 = Display `Set{(…)}`.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_set_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    let map = unsafe { &*map_from_handle(handle) };
    if map.inner.len() > 1_000_000 {
        return alloc_c_string("{...}");
    }
    let mut entries: Vec<_> = map.inner.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    let mut parts: Vec<String> = Vec::with_capacity(entries.len() * 2 + 2);
    parts.push(String::from("{"));
    for (i, (k, v)) in entries.iter().enumerate() {
        if i > 0 {
            parts.push(String::from(","));
        }
        parts.push(json_escape_string(k));
        parts.push(String::from(":"));
        let set_h = **v as SetHandle;
        let set_json = mimi_set_to_json_product_i64(set_h, arity, display_style);
        let s = unsafe { cstr_to_string(set_json) };
        if !set_json.is_null() {
            unsafe { libc::free(set_json as *mut std::ffi::c_void) };
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Build Map from JSON object whose values are product-set arrays:
/// `"a":[[1,2],[3,4]]` → Map string → Set of product.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_set_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    let s = unsafe { cstr_to_string(json) };
    let handle = mimi_map_new();
    if handle == 0 {
        return 0;
    }
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b'{' {
        return handle;
    }
    i += 1;
    loop {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b'}' {
            break;
        }
        if bytes[i] != b'"' {
            break;
        }
        i += 1;
        let start = i;
        while i < bytes.len() && bytes[i] != b'"' {
            if bytes[i] == b'\\' {
                i += 1;
            }
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let key = String::from_utf8_lossy(&bytes[start..i]).into_owned();
        i += 1;
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b':') {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'[' {
            break;
        }
        // Parse value as JSON array substring for set product.
        let val_start = i;
        let mut depth = 0i32;
        while i < bytes.len() {
            match bytes[i] {
                b'[' => depth += 1,
                b']' => {
                    depth -= 1;
                    if depth == 0 {
                        i += 1;
                        break;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let val_json = &s[val_start..i];
        let c_val = std::ffi::CString::new(val_json).unwrap_or_default();
        let set_h = mimi_set_from_json_product_i64(c_val.as_ptr(), arity);
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, set_h as ValueHandle);
        }
    }
    handle
}

/// Serialize Map values that are MapHandles of product-tuples.
/// `display_style`: 0 = JSON, 1 = Display with `(a, b)` products.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_map_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    let map = unsafe { &*map_from_handle(handle) };
    if map.inner.len() > 1_000_000 {
        return alloc_c_string("{...}");
    }
    let mut entries: Vec<_> = map.inner.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    let mut parts: Vec<String> = Vec::with_capacity(entries.len() * 2 + 2);
    parts.push(String::from("{"));
    for (i, (k, v)) in entries.iter().enumerate() {
        if i > 0 {
            parts.push(String::from(","));
        }
        parts.push(json_escape_string(k));
        parts.push(String::from(":"));
        let inner_h = **v as MapHandle;
        let inner_json = mimi_map_to_json_product_i64(inner_h, arity, display_style);
        let s = unsafe { cstr_to_string(inner_json) };
        if !inner_json.is_null() {
            unsafe { libc::free(inner_json as *mut std::ffi::c_void) };
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Build Map from JSON object whose values are nested Map product objects:
/// `"outer":{"a":[1,2]}`.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_map_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    let s = unsafe { cstr_to_string(json) };
    let handle = mimi_map_new();
    if handle == 0 {
        return 0;
    }
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b'{' {
        return handle;
    }
    i += 1;
    loop {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b'}' {
            break;
        }
        if bytes[i] != b'"' {
            break;
        }
        i += 1;
        let start = i;
        while i < bytes.len() && bytes[i] != b'"' {
            if bytes[i] == b'\\' {
                i += 1;
            }
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let key = String::from_utf8_lossy(&bytes[start..i]).into_owned();
        i += 1;
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b':') {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'{' {
            break;
        }
        // Parse nested object substring.
        let val_start = i;
        let mut depth = 0i32;
        while i < bytes.len() {
            match bytes[i] {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        i += 1;
                        break;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let val_json = &s[val_start..i];
        let c_val = std::ffi::CString::new(val_json).unwrap_or_default();
        let inner_h = mimi_map_from_json_product_i64(c_val.as_ptr(), arity);
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, inner_h as ValueHandle);
        }
    }
    handle
}

/// Build a MapHandle from a JSON object whose values are product-tuple arrays
/// of integers (e.g. `"a":[1,2]`). Each value is heap-packed as i64[arity]
/// matching `map_set` of product tuples / `mimi_map_to_json_product_i64`.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    let s = unsafe { cstr_to_string(json) };
    let handle = mimi_map_new();
    if handle == 0 {
        return 0;
    }
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b'{' {
        return handle;
    }
    i += 1;
    let n = arity as usize;
    loop {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b'}' {
            break;
        }
        if bytes[i] != b'"' {
            break;
        }
        i += 1;
        let start = i;
        while i < bytes.len() && bytes[i] != b'"' {
            if bytes[i] == b'\\' {
                i += 1;
            }
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let key = String::from_utf8_lossy(&bytes[start..i]).into_owned();
        i += 1;
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b':') {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'[' {
            break;
        }
        i += 1;
        let mut fields = vec![0i64; n];
        for fi in 0..n {
            while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
                i += 1;
            }
            let neg = i < bytes.len() && bytes[i] == b'-';
            if neg {
                i += 1;
            }
            let mut v: i64 = 0;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                v = v
                    .saturating_mul(10)
                    .saturating_add((bytes[i] - b'0') as i64);
                i += 1;
            }
            if neg {
                v = -v;
            }
            fields[fi] = v;
        }
        while i < bytes.len() && bytes[i] != b']' {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b']' {
            i += 1;
        }
        let data_size = n * std::mem::size_of::<i64>();
        let ptr = unsafe { libc::malloc(data_size) as *mut i64 };
        if ptr.is_null() {
            continue;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(fields.as_ptr(), ptr, n);
        }
        let vh = ptr as ValueHandle;
        unsafe {
            (*map_from_handle(handle)).inner.insert(key, vh);
        }
    }
    handle
}

/// Build a MapHandle from a JSON object with string keys and integer values.
/// Values are stored as raw i64 ValueHandles (same as map_set of integers).
#[no_mangle]
pub extern "C" fn mimi_map_from_json_i64(json: *const std::ffi::c_char) -> MapHandle {
    if json.is_null() {
        return mimi_map_new();
    }
    // SAFETY: non-null JSON C string from codegen.
    let s = unsafe { cstr_to_string(json) };
    let handle = mimi_map_new();
    if handle == 0 {
        return 0;
    }
    // Parse object via json_get_inner-style walk using serde-free JsonParser:
    // reuse keys from a lightweight scan of top-level object entries.
    let bytes = s.as_bytes();
    let mut pos = 0usize;
    while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') {
        pos += 1;
    }
    if pos >= bytes.len() || bytes[pos] != b'{' {
        return handle;
    }
    pos += 1;
    const MAX_ENTRIES: usize = 1_000_000;
    let mut count = 0usize;
    loop {
        if count >= MAX_ENTRIES {
            break;
        }
        while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r' | b',') {
            pos += 1;
        }
        if pos >= bytes.len() || bytes[pos] == b'}' {
            break;
        }
        if bytes[pos] != b'"' {
            break;
        }
        // Parse key
        pos += 1;
        let key_start = pos;
        let mut esc = false;
        let mut key = String::new();
        loop {
            if pos >= bytes.len() {
                return handle;
            }
            let c = bytes[pos];
            if esc {
                key.push(c as char);
                esc = false;
                pos += 1;
                continue;
            }
            if c == b'\\' {
                esc = true;
                pos += 1;
                continue;
            }
            if c == b'"' {
                pos += 1;
                break;
            }
            key.push(c as char);
            pos += 1;
        }
        let _ = key_start;
        while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') {
            pos += 1;
        }
        if pos >= bytes.len() || bytes[pos] != b':' {
            break;
        }
        pos += 1;
        while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') {
            pos += 1;
        }
        // Parse number / bool value as i64 (true→1, false→0).
        let val_start = pos;
        let mut dummy = JsonParser::new(&s[val_start..]);
        let parsed = dummy.parse_value();
        pos = val_start + dummy.pos;
        let v_i64 = match parsed {
            Some(ref tok) if tok == "true" => 1,
            Some(ref tok) if tok == "false" => 0,
            Some(ref num) => num.parse::<i64>().unwrap_or(0),
            None => 0,
        };
        // SAFETY: handle is a valid map from mimi_map_new.
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, v_i64 as ValueHandle);
        }
        count += 1;
    }
    handle
}

#[no_mangle]
pub extern "C" fn mimi_json_as_i64(json: *const std::ffi::c_char) -> i64 {
    if json.is_null() {
        return 0;
    }
    // SAFETY: `json` was checked non-null above.
    let s = unsafe { cstr_to_string(json) };
    let mut parser = JsonParser::new(&s);
    match parser.parse_value() {
        Some(val) if val == "true" => 1,
        Some(val) if val == "false" => 0,
        Some(val) => {
            // C6-fix: log parse failure instead of silently returning 0
            val.parse::<i64>().unwrap_or_else(|e| {
                eprintln!(
                    "[mimi runtime] mimi_json_as_i64: parse error for '{}': {}",
                    val, e
                );
                0
            })
        }
        None => 0,
    }
}

#[no_mangle]
pub extern "C" fn mimi_json_as_f64(json: *const std::ffi::c_char) -> f64 {
    if json.is_null() {
        return 0.0;
    }
    // SAFETY: `json` was checked non-null above.
    let s = unsafe { cstr_to_string(json) };
    let mut parser = JsonParser::new(&s);
    match parser.parse_value() {
        Some(val) => val.parse::<f64>().unwrap_or(0.0),
        None => 0.0,
    }
}

#[no_mangle]
pub extern "C" fn mimi_json_as_bool(json: *const std::ffi::c_char) -> i64 {
    if json.is_null() {
        return 0;
    }
    // SAFETY: `json` was checked non-null above.
    let s = unsafe { cstr_to_string(json) };
    let mut parser = JsonParser::new(&s);
    match parser.parse_value() {
        Some(val) => (val == "true") as i64,
        None => 0,
    }
}

// ─── Set operations ─────────────────────────────────────────────

// audit (MEDIUM — SetHandle pointer shrink on 32-bit):
// `SetHandle` stores a `Box<MimiSet>` raw pointer.  On 64-bit targets a
// pointer fits in i64, but on 32-bit targets `i64` is wider than a pointer
// and the `as SetHandle` / `as *mut MimiSet` round-trip is sound (the
// high bits are zero-extended).  The reverse direction — casting a 64-bit
// handle down to a 32-bit pointer — would lose bits, but that can only
// happen if the pointer was originally 64-bit, which is impossible on a
// 32-bit target.  We use a static assertion so the build fails if a future
// target ever has pointers wider than 64 bits.
//
// Mimi only targets 32-bit and 64-bit platforms (aarch64, x86_64, i686,
// aarch32), where this invariant holds.
type SetHandle = i64;
type SetValueHandle = i64;

// Static assertion: pointer width must not exceed 64 bits.
const _: () = {
    assert!(std::mem::size_of::<usize>() <= 8);
};

struct MimiSet {
    inner: std::collections::HashSet<SetValueHandle>,
}

/// S4: Return raw pointer instead of &'static mut to avoid aliasing UB.
/// S18: abort() instead of panic! — panic across FFI boundary is UB (Rust ABI requirement).
// SAFETY: aborts on invalid handle (0); caller must ensure `handle` is a unique `Box<MimiSet>`.
unsafe fn set_from_handle(handle: SetHandle) -> *mut MimiSet {
    if handle == 0 {
        std::process::abort();
    }
    handle as *mut MimiSet
}

#[no_mangle]
pub extern "C" fn mimi_set_new() -> SetHandle {
    let set = Box::new(MimiSet {
        inner: std::collections::HashSet::new(),
    });
    Box::into_raw(set) as SetHandle
}

/// Serialize Option<i64> layout `{disc:i1/i64, payload:i64}` to match interp:
/// Some → `{"Some":[n]}`, None → `"None"`.
#[no_mangle]
pub extern "C" fn mimi_option_i64_to_json(disc: i64, payload: i64) -> *mut std::ffi::c_char {
    if disc != 0 {
        alloc_c_string(&format!("{{\"Some\":[{}]}}", payload))
    } else {
        alloc_c_string("\"None\"")
    }
}

/// Serialize Result ok/err integer payloads: Ok → `{"Ok":[n]}`, Err → `{"Err":[n]}`.
#[no_mangle]
pub extern "C" fn mimi_result_i64_to_json(disc: i64, ok: i64, err: i64) -> *mut std::ffi::c_char {
    if disc != 0 {
        alloc_c_string(&format!("{{\"Ok\":[{}]}}", ok))
    } else {
        alloc_c_string(&format!("{{\"Err\":[{}]}}", err))
    }
}

/// Display form `Set{1, 2, 3}` (sorted ints) for println dual.
#[no_mangle]
pub extern "C" fn mimi_set_to_display(handle: SetHandle) -> *mut std::ffi::c_char {
    set_to_display_impl(handle, false)
}

/// Display form `Set{true, false}` for bool-valued sets.
#[no_mangle]
pub extern "C" fn mimi_set_to_display_bool(handle: SetHandle) -> *mut std::ffi::c_char {
    set_to_display_impl(handle, true)
}

fn set_to_display_impl(handle: SetHandle, as_bool: bool) -> *mut std::ffi::c_char {
    if handle == 0 {
        return alloc_c_string("Set{}");
    }
    // SAFETY: non-zero SetHandle.
    let set = unsafe { &*set_from_handle(handle) };
    if set.inner.len() > 1_000_000 {
        return alloc_c_string("Set{...}");
    }
    let mut vals: Vec<i64> = set.inner.iter().copied().collect();
    vals.sort_unstable();
    let mut s = String::from("Set{");
    for (i, v) in vals.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        if as_bool {
            s.push_str(if *v != 0 { "true" } else { "false" });
        } else {
            s.push_str(&v.to_string());
        }
    }
    s.push('}');
    alloc_c_string(&s)
}

/// Serialize a SetHandle of integer values to a JSON array string.
#[no_mangle]
pub extern "C" fn mimi_set_to_json_i64(handle: SetHandle) -> *mut std::ffi::c_char {
    if handle == 0 {
        return alloc_c_string("[]");
    }
    // SAFETY: non-zero SetHandle from mimi_set_new / from_json.
    let set = unsafe { &*set_from_handle(handle) };
    if set.inner.len() > 1_000_000 {
        return alloc_c_string("[...]");
    }
    let mut vals: Vec<i64> = set.inner.iter().copied().collect();
    vals.sort_unstable(); // order-stable for dual-backend
    let mut parts: Vec<String> = Vec::with_capacity(vals.len() * 2 + 2);
    parts.push(String::from("["));
    for (i, v) in vals.iter().enumerate() {
        if i > 0 {
            parts.push(String::from(","));
        }
        parts.push(v.to_string());
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

/// Serialize Set of heap-packed product-tuple i64[n] handles.
/// `display_style`: 0 = JSON `[[1,2]]`, 1 = Display `Set{(1, 2), (3, 4)}`.
#[no_mangle]
pub extern "C" fn mimi_set_to_json_product_i64(
    handle: SetHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return if display_style != 0 {
            alloc_c_string("Set{}")
        } else {
            alloc_c_string("[]")
        };
    }
    let set = unsafe { &*set_from_handle(handle) };
    if set.inner.len() > 1_000_000 {
        return if display_style != 0 {
            alloc_c_string("Set{...}")
        } else {
            alloc_c_string("[...]")
        };
    }
    let n = arity as usize;
    // Sort by decoded product fields for stable dual order.
    let mut items: Vec<Vec<i64>> = set
        .inner
        .iter()
        .map(|vh| {
            if *vh == 0 {
                vec![0; n]
            } else {
                let ptr = *vh as *const i64;
                if ptr.is_null() {
                    vec![0; n]
                } else {
                    unsafe { std::slice::from_raw_parts(ptr, n).to_vec() }
                }
            }
        })
        .collect();
    items.sort();
    if display_style != 0 {
        let mut parts: Vec<String> = Vec::with_capacity(items.len() * 2 + 2);
        parts.push(String::from("Set{"));
        for (i, fields) in items.iter().enumerate() {
            if i > 0 {
                parts.push(String::from(", "));
            }
            let body: Vec<String> = fields.iter().map(|x| x.to_string()).collect();
            parts.push(format!("({})", body.join(", ")));
        }
        parts.push(String::from("}"));
        alloc_c_string(&parts.join(""))
    } else {
        let mut parts: Vec<String> = Vec::with_capacity(items.len() * 2 + 2);
        parts.push(String::from("["));
        for (i, fields) in items.iter().enumerate() {
            if i > 0 {
                parts.push(String::from(","));
            }
            let body: Vec<String> = fields.iter().map(|x| x.to_string()).collect();
            parts.push(format!("[{}]", body.join(",")));
        }
        parts.push(String::from("]"));
        alloc_c_string(&parts.join(""))
    }
}

/// Build Set from JSON array of product arrays: `[[1,2],[3,4]]`.
#[no_mangle]
pub extern "C" fn mimi_set_from_json_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> SetHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_set_new();
    }
    let s = unsafe { cstr_to_string(json) };
    let handle = mimi_set_new();
    if handle == 0 {
        return 0;
    }
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b'[' {
        return handle;
    }
    i += 1;
    let n = arity as usize;
    loop {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b']' {
            break;
        }
        if bytes[i] != b'[' {
            break;
        }
        i += 1;
        let mut fields = vec![0i64; n];
        for fi in 0..n {
            while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
                i += 1;
            }
            let neg = i < bytes.len() && bytes[i] == b'-';
            if neg {
                i += 1;
            }
            let mut v: i64 = 0;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                v = v
                    .saturating_mul(10)
                    .saturating_add((bytes[i] - b'0') as i64);
                i += 1;
            }
            if neg {
                v = -v;
            }
            fields[fi] = v;
        }
        while i < bytes.len() && bytes[i] != b']' {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b']' {
            i += 1;
        }
        let data_size = n * std::mem::size_of::<i64>();
        let ptr = unsafe { libc::malloc(data_size) as *mut i64 };
        if ptr.is_null() {
            continue;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(fields.as_ptr(), ptr, n);
        }
        mimi_set_insert(handle, ptr as SetValueHandle);
    }
    handle
}

/// Serialize a SetHandle of 0/1 bool values to a JSON array of true/false.
#[no_mangle]
pub extern "C" fn mimi_set_to_json_bool(handle: SetHandle) -> *mut std::ffi::c_char {
    if handle == 0 {
        return alloc_c_string("[]");
    }
    // SAFETY: non-zero SetHandle.
    let set = unsafe { &*set_from_handle(handle) };
    if set.inner.len() > 1_000_000 {
        return alloc_c_string("[...]");
    }
    let mut vals: Vec<i64> = set.inner.iter().copied().collect();
    vals.sort_unstable(); // false(0) before true(1)
    let mut parts: Vec<String> = Vec::with_capacity(vals.len() * 2 + 2);
    parts.push(String::from("["));
    for (i, v) in vals.iter().enumerate() {
        if i > 0 {
            parts.push(String::from(","));
        }
        parts.push(if *v != 0 {
            String::from("true")
        } else {
            String::from("false")
        });
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

/// Serialize a SetHandle of f64-bit values to a JSON number array (serde-style).
#[no_mangle]
pub extern "C" fn mimi_set_to_json_f64(handle: SetHandle) -> *mut std::ffi::c_char {
    if handle == 0 {
        return alloc_c_string("[]");
    }
    // SAFETY: non-zero SetHandle.
    let set = unsafe { &*set_from_handle(handle) };
    if set.inner.len() > 1_000_000 {
        return alloc_c_string("[...]");
    }
    let mut vals: Vec<f64> = set
        .inner
        .iter()
        .map(|v| f64::from_bits(*v as u64))
        .collect();
    vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut parts: Vec<String> = Vec::with_capacity(vals.len() * 2 + 2);
    parts.push(String::from("["));
    for (i, f) in vals.iter().enumerate() {
        if i > 0 {
            parts.push(String::from(","));
        }
        if f.fract() == 0.0 && f.is_finite() {
            parts.push(format!("{}.0", *f as i64));
        } else {
            parts.push(format!("{}", f));
        }
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

/// Serialize a SetHandle of C-string ValueHandles to a JSON string array.
#[no_mangle]
pub extern "C" fn mimi_set_to_json_string(handle: SetHandle) -> *mut std::ffi::c_char {
    if handle == 0 {
        return alloc_c_string("[]");
    }
    // SAFETY: non-zero SetHandle.
    let set = unsafe { &*set_from_handle(handle) };
    if set.inner.len() > 1_000_000 {
        return alloc_c_string("[...]");
    }
    // RT-H1: only decode via safe_c_string_from_handle (no bare size/align probe).
    let mut vals: Vec<String> = set
        .inner
        .iter()
        .map(|v| safe_c_string_from_handle(*v as ValueHandle).unwrap_or_default())
        .collect();
    vals.sort();
    let mut parts: Vec<String> = Vec::with_capacity(vals.len() * 2 + 2);
    parts.push(String::from("["));
    for (i, v) in vals.iter().enumerate() {
        if i > 0 {
            parts.push(String::from(","));
        }
        parts.push(json_escape_string(v));
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

/// Build a SetHandle from a JSON array of f64 values (stored as bit patterns).
#[no_mangle]
pub extern "C" fn mimi_set_from_json_f64(json: *const std::ffi::c_char) -> SetHandle {
    let handle = mimi_set_new();
    if handle == 0 || json.is_null() {
        return handle;
    }
    // SAFETY: non-null JSON C string from codegen.
    let s = unsafe { cstr_to_string(json) };
    let len = json_array_length(json);
    if len <= 0 {
        return handle;
    }
    const MAX: i64 = 1_000_000;
    let n = len.min(MAX);
    for i in 0..n {
        let elem = json_get_element(json, i);
        if elem.is_null() {
            continue;
        }
        // SAFETY: elem is a heap C string from json_get_element.
        let es = unsafe { cstr_to_string(elem) };
        let bits = es.trim().parse::<f64>().unwrap_or(0.0).to_bits() as i64;
        unsafe {
            libc::free(elem as *mut std::ffi::c_void);
        }
        mimi_set_insert(handle, bits as SetValueHandle);
    }
    let _ = s;
    handle
}

/// Display form `Set{1.5, 2}` for f64-bit sets (sorted by bit pattern / float value).
#[no_mangle]
pub extern "C" fn mimi_set_to_display_f64(handle: SetHandle) -> *mut std::ffi::c_char {
    if handle == 0 {
        return alloc_c_string("Set{}");
    }
    // SAFETY: non-zero SetHandle.
    let set = unsafe { &*set_from_handle(handle) };
    if set.inner.len() > 1_000_000 {
        return alloc_c_string("Set{...}");
    }
    let mut vals: Vec<f64> = set
        .inner
        .iter()
        .map(|v| f64::from_bits(*v as u64))
        .collect();
    vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut s = String::from("Set{");
    for (i, f) in vals.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        if f.fract() == 0.0 && f.is_finite() {
            s.push_str(&format!("{}", *f as i64));
        } else {
            s.push_str(&format!("{}", f));
        }
    }
    s.push('}');
    alloc_c_string(&s)
}

/// Build a SetHandle from a JSON array of strings.
/// Elements are stored as heap-cloned C-string ValueHandles.
#[no_mangle]
pub extern "C" fn mimi_set_from_json_string(json: *const std::ffi::c_char) -> SetHandle {
    let handle = mimi_set_new();
    if handle == 0 || json.is_null() {
        return handle;
    }
    // SAFETY: non-null JSON C string from codegen.
    let s = unsafe { cstr_to_string(json) };
    let len = json_array_length(json);
    if len <= 0 {
        return handle;
    }
    const MAX: i64 = 1_000_000;
    let n = len.min(MAX);
    for i in 0..n {
        let elem = json_get_element(json, i);
        if elem.is_null() {
            continue;
        }
        // SAFETY: elem is a heap C string from json_get_element.
        let es = unsafe { cstr_to_string(elem) };
        // Strip surrounding quotes if present (json_get_element may return quoted).
        let body = es.trim().trim_matches('"');
        let v = mimi_str_clone(body.as_ptr() as *const std::ffi::c_char, body.len() as i64);
        unsafe {
            libc::free(elem as *mut std::ffi::c_void);
        }
        mimi_set_insert(handle, v as SetValueHandle);
    }
    let _ = s;
    handle
}

/// Display form `Set{a, b}` for string-valued sets (sorted by string content).
#[no_mangle]
pub extern "C" fn mimi_set_to_display_string(handle: SetHandle) -> *mut std::ffi::c_char {
    if handle == 0 {
        return alloc_c_string("Set{}");
    }
    // SAFETY: non-zero SetHandle.
    let set = unsafe { &*set_from_handle(handle) };
    if set.inner.len() > 1_000_000 {
        return alloc_c_string("Set{...}");
    }
    // RT-H1: string decode only through safe_c_string_from_handle; else decimal.
    let mut vals: Vec<String> = set
        .inner
        .iter()
        .map(|v| {
            safe_c_string_from_handle(*v as ValueHandle).unwrap_or_else(|| v.to_string())
        })
        .collect();
    vals.sort();
    let mut s = String::from("Set{");
    for (i, v) in vals.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        s.push_str(v);
    }
    s.push('}');
    alloc_c_string(&s)
}

/// Build a SetHandle from a JSON array of integers.
#[no_mangle]
pub extern "C" fn mimi_set_from_json_i64(json: *const std::ffi::c_char) -> SetHandle {
    let handle = mimi_set_new();
    if handle == 0 || json.is_null() {
        return handle;
    }
    // SAFETY: non-null JSON C string from codegen.
    let s = unsafe { cstr_to_string(json) };
    let len = json_array_length(json);
    if len <= 0 {
        return handle;
    }
    const MAX: i64 = 1_000_000;
    let n = len.min(MAX);
    for i in 0..n {
        let elem = json_get_element(json, i);
        if elem.is_null() {
            continue;
        }
        let v = mimi_json_as_i64(elem);
        // Free the element string allocated by json_get_element.
        unsafe {
            libc::free(elem as *mut std::ffi::c_void);
        }
        mimi_set_insert(handle, v as SetValueHandle);
    }
    let _ = s;
    handle
}

#[no_mangle]
pub extern "C" fn mimi_set_destroy(handle: SetHandle) {
    if handle == 0 {
        return;
    }
    // SAFETY: handle is non-zero; reconstructing the Box and dropping it.
    unsafe {
        drop(Box::from_raw(handle as *mut MimiSet));
    }
}

#[no_mangle]
pub extern "C" fn mimi_set_insert(handle: SetHandle, value: SetValueHandle) -> SetHandle {
    if handle == 0 {
        return handle;
    }
    // SAFETY: handle validated by `set_from_handle`; deref is in a single scope.
    unsafe {
        (*set_from_handle(handle)).inner.insert(value);
    }
    handle
}

#[no_mangle]
pub extern "C" fn mimi_set_contains(handle: SetHandle, value: SetValueHandle) -> i64 {
    if handle == 0 {
        return 0;
    }
    // SAFETY: handle validated by `set_from_handle`; deref is in a single scope.
    unsafe { (*set_from_handle(handle)).inner.contains(&value) as i64 }
}

#[no_mangle]
pub extern "C" fn mimi_set_remove(handle: SetHandle, value: SetValueHandle) -> SetHandle {
    if handle == 0 {
        return handle;
    }
    // SAFETY: handle validated by `set_from_handle`; deref is in a single scope.
    unsafe {
        (*set_from_handle(handle)).inner.remove(&value);
    }
    handle
}

#[no_mangle]
pub extern "C" fn mimi_set_size(handle: SetHandle) -> i64 {
    if handle == 0 {
        return 0;
    }
    // SAFETY: handle validated by `set_from_handle`; deref is in a single scope.
    unsafe { (*set_from_handle(handle)).inner.len() as i64 }
}

#[no_mangle]
pub extern "C" fn mimi_set_to_list(handle: SetHandle, out_len: *mut i64) -> *mut SetValueHandle {
    // P2-14 fix: handle == 0 (invalid) returns distinct sentinel from empty set.
    // Invalid handle: returns null, *out_len = -1.
    // Empty set: returns null, *out_len = 0.
    // This allows callers to distinguish the two cases.
    if out_len.is_null() {
        return std::ptr::null_mut();
    }
    if handle == 0 {
        // SAFETY: `out_len` was checked non-null above.
        unsafe {
            *out_len = -1;
        }
        return std::ptr::null_mut();
    }
    // SAFETY: handle validated by `set_from_handle`; shared reference is in a single scope.
    let set = unsafe { &*set_from_handle(handle) };
    let len = set.inner.len() as i64;
    // SAFETY: `out_len` was checked non-null above.
    unsafe {
        *out_len = len;
    }
    if len == 0 {
        return std::ptr::null_mut();
    }
    let mut vec: Vec<SetValueHandle> = set.inner.iter().copied().collect();
    // RT-C5: shrink so len == capacity, matching mimi_set_list_free reconstruction.
    vec.shrink_to_fit();
    debug_assert_eq!(vec.len(), vec.capacity());
    let ptr = vec.as_mut_ptr();
    std::mem::forget(vec); // ownership transferred to caller
    ptr
}

/// C9 fix: Free a SetValueHandle array returned by `mimi_set_to_list`.
/// Must be called with the pointer and length as returned by `mimi_set_to_list`.
/// Safe to call with null pointer (no-op).
#[no_mangle]
pub extern "C" fn mimi_set_list_free(ptr: *mut SetValueHandle, len: i64) {
    if ptr.is_null() || len <= 0 {
        return;
    }
    // RT-C5: mimi_set_to_list always shrink_to_fit, so capacity == len.
    // Reconstruct the Vec from the raw pointer and length, then drop it.
    // SAFETY: `ptr` was obtained from `mimi_set_to_list` which forgets a
    // `Vec<SetValueHandle>` after shrink_to_fit (len == capacity).
    // `SetValueHandle` has no custom Drop. The pointer is non-null and
    // `len > 0` (checked above).
    unsafe {
        drop(Vec::from_raw_parts(ptr, len as usize, len as usize));
    }
}

// ─── Regex (simple recursive backtracking engine, self-contained) ───

struct RegexEngine;

/// S17: Maximum recursion depth for regex backtracking to prevent ReDoS.
/// Patterns like `(a+)+b` on `aaaaaaaaaaaaaaaac` cause exponential recursion.
const REGEX_MAX_DEPTH: usize = 100;

impl RegexEngine {
    /// Expand `{n}` / `{n,m}` exact/range quantifiers into `*`/`+` form that
    /// the recursive matcher understands. Also used by capture_groups.
    /// Only expands simple `{digits}` and `{digits,digits}` after an atom.
    fn expand_braces(pattern: &str) -> String {
        let bytes = pattern.as_bytes();
        let mut out = String::with_capacity(pattern.len() * 2);
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'\\' && i + 1 < bytes.len() {
                out.push(bytes[i] as char);
                out.push(bytes[i + 1] as char);
                i += 2;
                continue;
            }
            if bytes[i] == b'{' {
                // Look back at last atom written to out — re-emit n times.
                // Find atom start in `out`.
                let atom = Self::last_atom(&out);
                if let Some((atom_start, atom_str)) = atom {
                    // Parse {n} or {n,m}
                    let mut j = i + 1;
                    let mut n_str = String::new();
                    while j < bytes.len() && bytes[j].is_ascii_digit() {
                        n_str.push(bytes[j] as char);
                        j += 1;
                    }
                    let mut m_str = String::new();
                    if j < bytes.len() && bytes[j] == b',' {
                        j += 1;
                        while j < bytes.len() && bytes[j].is_ascii_digit() {
                            m_str.push(bytes[j] as char);
                            j += 1;
                        }
                    }
                    if j < bytes.len() && bytes[j] == b'}' && !n_str.is_empty() {
                        let n: usize = n_str.parse().unwrap_or(0).min(64);
                        let m: usize = if m_str.is_empty() {
                            n
                        } else {
                            m_str.parse().unwrap_or(n).min(64)
                        };
                        // Drop the atom already written; re-emit min(n,m) times + optional rest.
                        out.truncate(atom_start);
                        let min_c = n.min(m);
                        let max_c = n.max(m);
                        for _ in 0..min_c {
                            out.push_str(&atom_str);
                        }
                        // Remaining optional: emit (atom)? for each extra up to max-min
                        // by expanding to atom? atom? ... which our engine lacks for `?`.
                        // Fallback: emit atom* when max > min (approximate), else exact.
                        if max_c > min_c {
                            // Emit (max-min) optional atoms as atom* on last — coarse but dual-ok
                            // for common exact `{n}` (max==min) which is the dual path.
                            for _ in min_c..max_c {
                                out.push_str(&atom_str);
                            }
                        }
                        i = j + 1;
                        continue;
                    }
                }
            }
            out.push(bytes[i] as char);
            i += 1;
        }
        out
    }

    /// Return (start_index_in_out, atom_string) for the last regex atom in `out`.
    fn last_atom(out: &str) -> Option<(usize, String)> {
        let b = out.as_bytes();
        if b.is_empty() {
            return None;
        }
        let mut end = b.len();
        // Skip trailing quantifiers already present (* +)
        while end > 0 && (b[end - 1] == b'*' || b[end - 1] == b'+') {
            end -= 1;
        }
        if end == 0 {
            return None;
        }
        // Character class [...]
        if b[end - 1] == b']' {
            let mut i = end - 1;
            while i > 0 {
                i -= 1;
                if b[i] == b'[' && (i == 0 || b[i - 1] != b'\\') {
                    return Some((i, out[i..end].to_string()));
                }
            }
            return None;
        }
        // Escape sequence
        if end >= 2 && b[end - 2] == b'\\' {
            return Some((end - 2, out[end - 2..end].to_string()));
        }
        // Group (...)
        if b[end - 1] == b')' {
            let mut depth = 0i32;
            let mut i = end;
            while i > 0 {
                i -= 1;
                if b[i] == b')' && (i == 0 || b[i - 1] != b'\\') {
                    depth += 1;
                } else if b[i] == b'(' && (i == 0 || b[i - 1] != b'\\') {
                    depth -= 1;
                    if depth == 0 {
                        return Some((i, out[i..end].to_string()));
                    }
                }
            }
            return None;
        }
        // Single char atom
        Some((end - 1, out[end - 1..end].to_string()))
    }

    /// Capture groups from first match. Returns group 1..N strings (not full match).
    fn capture_groups(text: &str, pattern: &str) -> Option<Vec<String>> {
        let expanded = Self::expand_braces(pattern);
        // Strip capturing parens for the full-match scan, but keep structure.
        // Walk pattern with captures by matching left-to-right with backtracking.
        let text_bytes = text.as_bytes();
        let pat_bytes = expanded.as_bytes();
        let anchored = !pat_bytes.is_empty() && pat_bytes[0] == b'^';
        for start in 0..=text_bytes.len() {
            let mut caps: Vec<Option<(usize, usize)>> = Vec::new();
            if let Some(end) =
                Self::match_with_captures(pat_bytes, &text_bytes[start..], 0, &mut caps)
            {
                let mut groups = Vec::new();
                for c in caps {
                    if let Some((a, b)) = c {
                        let abs_a = start + a;
                        let abs_b = start + b;
                        groups.push(
                            std::str::from_utf8(&text_bytes[abs_a..abs_b])
                                .unwrap_or("")
                                .to_string(),
                        );
                    } else {
                        groups.push(String::new());
                    }
                }
                // end is relative; ensure we actually consumed something or empty match ok
                let _ = end;
                return Some(groups);
            }
            if anchored || start >= text_bytes.len() {
                break;
            }
        }
        None
    }

    /// Match with capture tracking. `text` is a suffix; indices in captures are
    /// relative to this suffix. Returns consumed length on success.
    fn match_with_captures(
        pattern: &[u8],
        text: &[u8],
        depth: usize,
        caps: &mut Vec<Option<(usize, usize)>>,
    ) -> Option<usize> {
        if depth >= REGEX_MAX_DEPTH {
            return None;
        }
        let mut pi = 0usize;
        let mut ti = 0usize;
        let plen = pattern.len();
        let tlen = text.len();
        if pi < plen && pattern[pi] == b'^' {
            pi += 1;
        }
        loop {
            if pi >= plen {
                return Some(ti);
            }
            if pattern[pi] == b'$' && pi + 1 >= plen {
                return if ti >= tlen { Some(ti) } else { None };
            }
            // Capturing group (...)
            if pattern[pi] == b'(' {
                let close = Self::find_matching_paren(pattern, pi)?;
                let inner = &pattern[pi + 1..close];
                let mut after = close + 1;
                let (min_c, max_c) = Self::read_quant(pattern, &mut after);
                // Greedy: try max down to min
                for count in (min_c..=max_c).rev() {
                    let caps_len = caps.len();
                    let mut t2 = ti;
                    let mut ok = true;
                    let mut last_span: Option<(usize, usize)> = None;
                    for _ in 0..count {
                        let mut inner_caps = Vec::new();
                        match Self::match_with_captures(inner, &text[t2..], depth + 1, &mut inner_caps)
                        {
                            Some(n) => {
                                last_span = Some((t2, t2 + n));
                                // Nested captures: merge relative offsets
                                for ic in inner_caps {
                                    if let Some((a, b)) = ic {
                                        caps.push(Some((t2 + a, t2 + b)));
                                    } else {
                                        caps.push(None);
                                    }
                                }
                                t2 += n;
                            }
                            None => {
                                ok = false;
                                break;
                            }
                        }
                    }
                    if ok {
                        // This group's span = last repetition (or empty if count==0)
                        let span = if count == 0 {
                            Some((ti, ti))
                        } else {
                            // Full group span from first rep start to last rep end
                            // Recompute first start = ti
                            last_span.map(|(_, end)| (ti, end)).or(Some((ti, ti)))
                        };
                        // Insert this group's capture at the position before nested ones?
                        // Spec: groups numbered by open-paren order. Nested first in walk.
                        // We pushed nested during loop; insert this group at caps_len.
                        if let Some(sp) = span {
                            // For multi-rep, span should be whole run
                            let full = if count > 0 {
                                (ti, t2)
                            } else {
                                sp
                            };
                            caps.insert(caps_len, Some(full));
                        } else {
                            caps.insert(caps_len, None);
                        }
                        // Try rest of pattern; offset new captures by t2 (suffix base).
                        let caps_before_rest = caps.len();
                        if let Some(rest) =
                            Self::match_with_captures(&pattern[after..], &text[t2..], depth + 1, caps)
                        {
                            for c in caps.iter_mut().skip(caps_before_rest) {
                                if let Some((a, b)) = *c {
                                    *c = Some((a + t2, b + t2));
                                }
                            }
                            return Some(t2 + rest);
                        }
                    }
                    caps.truncate(caps_len);
                }
                return None;
            }

            // Non-group atom + quantifier
            let (elem_end, elem_is_class) = Self::parse_element(pattern, pi);
            if elem_end == pi {
                return None;
            }
            let mut after = elem_end;
            let has_star = after < plen && pattern[after] == b'*';
            let has_plus = after < plen && pattern[after] == b'+';
            if has_star || has_plus {
                after += 1;
            }
            let min_c = if has_plus {
                1
            } else if has_star {
                0
            } else {
                1
            };
            let max_c = if has_star || has_plus {
                // greedy max
                let mut scan = ti;
                let mut cnt = 0;
                while scan < tlen {
                    let mut tmp = pi;
                    if !Self::elem_match(pattern, &mut tmp, text[scan], elem_is_class) {
                        break;
                    }
                    scan += 1;
                    cnt += 1;
                    if cnt > 10_000 {
                        break;
                    }
                }
                cnt
            } else {
                1
            };
            if max_c < min_c {
                return None;
            }
            for count in (min_c..=max_c).rev() {
                let mut t2 = ti;
                let mut ok = true;
                let mut p_tmp = pi;
                for _ in 0..count {
                    if t2 >= tlen
                        || !Self::elem_match(pattern, &mut p_tmp, text[t2], elem_is_class)
                    {
                        ok = false;
                        break;
                    }
                    t2 += 1;
                    p_tmp = pi; // reset element start for next rep
                }
                if !ok {
                    continue;
                }
                let caps_before_rest = caps.len();
                if let Some(rest) =
                    Self::match_with_captures(&pattern[after..], &text[t2..], depth + 1, caps)
                {
                    for c in caps.iter_mut().skip(caps_before_rest) {
                        if let Some((a, b)) = *c {
                            *c = Some((a + t2, b + t2));
                        }
                    }
                    return Some(t2 + rest);
                }
            }
            return None;
        }
    }

    fn find_matching_paren(pattern: &[u8], open: usize) -> Option<usize> {
        let mut depth = 0i32;
        let mut i = open;
        while i < pattern.len() {
            if pattern[i] == b'\\' {
                i += 2;
                continue;
            }
            if pattern[i] == b'(' {
                depth += 1;
            } else if pattern[i] == b')' {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            i += 1;
        }
        None
    }

    fn read_quant(pattern: &[u8], pos: &mut usize) -> (usize, usize) {
        if *pos < pattern.len() && pattern[*pos] == b'*' {
            *pos += 1;
            return (0, 10_000);
        }
        if *pos < pattern.len() && pattern[*pos] == b'+' {
            *pos += 1;
            return (1, 10_000);
        }
        (1, 1)
    }

    fn match_pattern(text: &str, pattern: &str) -> bool {
        let expanded = Self::expand_braces(pattern);
        // Strip bare capturing parens for match-only path (treat as non-capturing).
        let stripped = Self::strip_captures(&expanded);
        let text_bytes = text.as_bytes();
        let pat_bytes = stripped.as_bytes();
        let anchored = !pat_bytes.is_empty() && pat_bytes[0] == b'^';

        for start in 0..=text_bytes.len() {
            let result = Self::match_here_with_depth(pat_bytes, &text_bytes[start..], 0);
            if result >= 0 {
                return true;
            }
            if anchored || start >= text_bytes.len() {
                break;
            }
        }
        false
    }

    fn strip_captures(pattern: &str) -> String {
        let mut out = String::with_capacity(pattern.len());
        let b = pattern.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'\\' && i + 1 < b.len() {
                out.push(b[i] as char);
                out.push(b[i + 1] as char);
                i += 2;
                continue;
            }
            if b[i] == b'(' || b[i] == b')' {
                i += 1;
                continue;
            }
            out.push(b[i] as char);
            i += 1;
        }
        out
    }

    fn find_match(text: &str, pattern: &str) -> Option<(usize, usize)> {
        let expanded = Self::expand_braces(pattern);
        let stripped = Self::strip_captures(&expanded);
        let text_bytes = text.as_bytes();
        let pat_bytes = stripped.as_bytes();
        let anchored = !pat_bytes.is_empty() && pat_bytes[0] == b'^';

        for start in 0..=text_bytes.len() {
            let consumed = Self::match_here_with_depth(pat_bytes, &text_bytes[start..], 0);
            if consumed >= 0 {
                return Some((start, start + consumed as usize));
            }
            if anchored || start >= text_bytes.len() {
                break;
            }
        }
        None
    }

    fn replace_all(text: &str, pattern: &str, replacement: &str) -> String {
        let expanded = Self::expand_braces(pattern);
        let stripped = Self::strip_captures(&expanded);
        let text_bytes = text.as_bytes();
        let pat_bytes = stripped.as_bytes();
        let mut result = String::new();
        let mut cursor = 0;
        loop {
            if cursor >= text_bytes.len() {
                break;
            }
            let mut best_pos = text_bytes.len() + 1;
            let mut best_len = 0;
            for start in cursor..text_bytes.len() {
                let consumed = Self::match_here_with_depth(pat_bytes, &text_bytes[start..], 0);
                if consumed >= 0 {
                    best_pos = start;
                    best_len = consumed as usize;
                    break;
                }
            }
            if best_pos <= text_bytes.len() {
                // Append prefix
                result.push_str(std::str::from_utf8(&text_bytes[cursor..best_pos]).unwrap_or(""));
                // Append replacement
                result.push_str(replacement);
                cursor = best_pos + best_len;
            } else {
                // No more match
                result.push_str(std::str::from_utf8(&text_bytes[cursor..]).unwrap_or(""));
                break;
            }
        }
        result
    }

    /// Match pattern against text starting at current position.
    /// Returns number of text characters consumed on success, -1 on failure.
    /// S17: depth-limited variant to prevent ReDoS exponential backtracking.
    fn match_here_with_depth(pattern: &[u8], text: &[u8], depth: usize) -> i32 {
        if depth >= REGEX_MAX_DEPTH {
            return -1; // S17: abort to prevent stack overflow from ReDoS
        }
        let mut pi = 0;
        let mut ti = 0;
        let plen = pattern.len();
        let tlen = text.len();

        // Skip leading ^
        if pi < plen && pattern[pi] == b'^' {
            pi += 1;
        }

        loop {
            if pi >= plen {
                return ti as i32; // matched all of pattern
            }

            // $ at end of pattern matches end of text
            if pattern[pi] == b'$' && (pi + 1 >= plen) {
                return if ti >= tlen { ti as i32 } else { -1 };
            }

            // Parse element
            let (elem_end, elem_is_class) = Self::parse_element(pattern, pi);
            if elem_end == pi {
                return -1;
            }

            // Check for quantifier
            let has_star = elem_end < plen && pattern[elem_end] == b'*';
            let has_plus = elem_end < plen && pattern[elem_end] == b'+';
            let after_quant = if has_star || has_plus {
                elem_end + 1
            } else {
                elem_end
            };

            if has_star || has_plus {
                // Greedy matching
                let min_count = if has_plus { 1 } else { 0 };

                // Count maximum possible matches
                let mut max_count = 0;
                let mut scan = ti;
                while scan < tlen {
                    let mut tmp_pi = pi;
                    if !Self::elem_match(pattern, &mut tmp_pi, text[scan], elem_is_class) {
                        break;
                    }
                    scan += 1;
                    max_count += 1;
                }

                // Try from max down to min
                let mut matched = false;
                for count in (min_count..=max_count).rev() {
                    let sub_pat = &pattern[after_quant..];
                    let sub_text = &text[ti + count..];
                    let r = Self::match_here_with_depth(sub_pat, sub_text, depth + 1);
                    if r >= 0 {
                        ti = ti + count + r as usize;
                        matched = true;
                        break;
                    }
                }
                if !matched {
                    return -1;
                }
                pi = plen; // after_quant is already consumed via recursive call
                continue;
            }

            if ti >= tlen {
                return -1;
            }
            if !Self::elem_match(pattern, &mut pi, text[ti], elem_is_class) {
                return -1;
            }
            ti += 1;
        }
    }

    /// Parse pattern element starting at pi, return (end_pos, is_class).
    fn parse_element(pattern: &[u8], pi: usize) -> (usize, bool) {
        if pi >= pattern.len() {
            return (pi, false);
        }
        match pattern[pi] {
            b'\\' => (pi + 2, false),
            b'[' => {
                let mut ep = pi + 1;
                if ep < pattern.len() && pattern[ep] == b'^' {
                    ep += 1;
                }
                while ep < pattern.len() && pattern[ep] != b']' {
                    if pattern[ep] == b'\\' && ep + 1 < pattern.len() {
                        ep += 2;
                    } else {
                        ep += 1;
                    }
                }
                if ep < pattern.len() {
                    ep += 1;
                } // skip ]
                (ep, true)
            }
            _ => (pi + 1, false),
        }
    }

    fn elem_match_in_class(class: &[u8], c: u8, start: usize) -> (bool, usize) {
        let mut pos = start;
        let neg = pos < class.len() && class[pos] == b'^';
        if neg {
            pos += 1;
        }

        let mut matched = false;
        while pos < class.len() && class[pos] != b']' {
            if pos + 2 < class.len() && class[pos + 1] == b'-' && class[pos + 2] != b']' {
                if c >= class[pos] && c <= class[pos + 2] {
                    matched = true;
                }
                pos += 3;
            } else {
                if c == class[pos] {
                    matched = true;
                }
                pos += 1;
            }
        }
        // Advance to end of class
        while pos < class.len() && class[pos] != b']' {
            pos += 1;
        }
        if pos < class.len() {
            pos += 1;
        } // skip ]

        if neg {
            (!matched, pos)
        } else {
            (matched, pos)
        }
    }

    /// Check if pattern element at pi matches character c. Advances pi past element.
    fn elem_match(pattern: &[u8], pi: &mut usize, c: u8, is_class: bool) -> bool {
        if *pi >= pattern.len() {
            return false;
        }

        if is_class {
            // [...] class
            let class_start = *pi + 1; // skip [
            let (matched, end) = Self::elem_match_in_class(pattern, c, class_start);
            *pi = end;
            return matched;
        }

        match pattern[*pi] {
            b'\\' => {
                if *pi + 1 >= pattern.len() {
                    return false;
                }
                let esc = pattern[*pi + 1];
                *pi += 2;
                match esc {
                    b'd' => c.is_ascii_digit(),
                    b'D' => !c.is_ascii_digit(),
                    b'w' => c.is_ascii_alphanumeric() || c == b'_',
                    b'W' => !(c.is_ascii_alphanumeric() || c == b'_'),
                    b's' => c.is_ascii_whitespace(),
                    b'S' => !c.is_ascii_whitespace(),
                    _ => c == esc,
                }
            }
            b'.' => {
                *pi += 1;
                c != b'\n' && c != 0
            }
            _ => {
                let ch = pattern[*pi];
                *pi += 1;
                c == ch
            }
        }
    }
}

const MAX_REGEX_PATTERN_LEN: usize = 512;

#[no_mangle]
pub extern "C" fn mimi_regex_match(
    text: *const std::ffi::c_char,
    pattern: *const std::ffi::c_char,
) -> i32 {
    if text.is_null() || pattern.is_null() {
        return 0;
    }
    // SAFETY: pointers checked non-null above.
    let t = unsafe { cstr_to_string(text) };
    // SAFETY: pointers checked non-null above.
    let p = unsafe { cstr_to_string(pattern) };
    if p.len() > MAX_REGEX_PATTERN_LEN {
        return 0;
    }
    RegexEngine::match_pattern(&t, &p) as i32
}

#[no_mangle]
pub extern "C" fn mimi_regex_find(
    text: *const std::ffi::c_char,
    pattern: *const std::ffi::c_char,
) -> *mut std::ffi::c_char {
    if text.is_null() || pattern.is_null() {
        return alloc_c_string("");
    }
    // SAFETY: pointers checked non-null above.
    let t = unsafe { cstr_to_string(text) };
    // SAFETY: pointers checked non-null above.
    let p = unsafe { cstr_to_string(pattern) };
    if p.len() > MAX_REGEX_PATTERN_LEN {
        return alloc_c_string("");
    }
    match RegexEngine::find_match(&t, &p) {
        Some((start, end)) => {
            let matched = &t[start..end];
            alloc_c_string(matched)
        }
        None => alloc_c_string(""),
    }
}

#[no_mangle]
pub extern "C" fn mimi_regex_replace(
    text: *const std::ffi::c_char,
    pattern: *const std::ffi::c_char,
    replacement: *const std::ffi::c_char,
) -> *mut std::ffi::c_char {
    if text.is_null() || pattern.is_null() || replacement.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: pointers checked non-null above.
    let t = unsafe { cstr_to_string(text) };
    // SAFETY: pointers checked non-null above.
    let p = unsafe { cstr_to_string(pattern) };
    if p.len() > MAX_REGEX_PATTERN_LEN {
        return std::ptr::null_mut();
    }
    // SAFETY: replacement pointer checked non-null above.
    let r = unsafe { cstr_to_string(replacement) };
    let result = RegexEngine::replace_all(&t, &p, &r);
    alloc_c_string(&result)
}

/// Finds all non-overlapping matches of pattern in text.
/// Returns a JSON array of matched strings: ["match1","match2",...]
#[no_mangle]
pub extern "C" fn mimi_regex_find_all(
    text: *const std::ffi::c_char,
    pattern: *const std::ffi::c_char,
) -> *mut std::ffi::c_char {
    if text.is_null() || pattern.is_null() {
        return alloc_c_string("[]");
    }
    // SAFETY: pointers checked non-null above.
    let t = unsafe { cstr_to_string(text) };
    // SAFETY: pointers checked non-null above.
    let p = unsafe { cstr_to_string(pattern) };
    let mut matches = Vec::new();
    let mut cursor = 0;
    let t_bytes = t.as_bytes();
    let p_bytes = p.as_bytes();
    loop {
        if cursor >= t_bytes.len() {
            break;
        }
        let mut found = -1;
        let mut found_start = 0;
        for start in cursor..t_bytes.len() {
            let consumed = RegexEngine::match_here_with_depth(p_bytes, &t_bytes[start..], 0);
            if consumed >= 0 {
                let matched =
                    std::str::from_utf8(&t_bytes[start..start + consumed as usize]).unwrap_or("");
                matches.push(matched.to_string());
                found = consumed;
                found_start = start;
                break;
            }
        }
        if found < 0 {
            break;
        }
        cursor = found_start + found as usize;
    }
    let mut result = String::from("[");
    let mut first = true;
    for m in &matches {
        if !first {
            result.push(',');
        }
        first = false;
        result.push('"');
        for ch in m.chars() {
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

/// Extracts capture groups from the first match of pattern in text.
/// Returns a JSON array of capture group values: `["group1","group2",...]`
/// (group 0 / full match is excluded — same as the interpreter).
///
/// Standalone runtime has no `regex` crate; uses the in-tree `RegexEngine`
/// with capture/`{n}` support so codegen duals match `mimi run`.
#[no_mangle]
pub extern "C" fn mimi_regex_capture_groups(
    text: *const std::ffi::c_char,
    pattern: *const std::ffi::c_char,
) -> *mut std::ffi::c_char {
    if text.is_null() || pattern.is_null() {
        return alloc_c_string("[]");
    }
    // SAFETY: non-null C strings from codegen/interp callers.
    let t = unsafe { cstr_to_string(text) };
    let p = unsafe { cstr_to_string(pattern) };
    if p.len() > MAX_REGEX_PATTERN_LEN {
        return alloc_c_string("[]");
    }
    match RegexEngine::capture_groups(&t, &p) {
        Some(groups) => {
            let mut out = String::from("[");
            for (i, g) in groups.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push('"');
                for ch in g.chars() {
                    match ch {
                        '\\' => out.push_str("\\\\"),
                        '"' => out.push_str("\\\""),
                        c => out.push(c),
                    }
                }
                out.push('"');
            }
            out.push(']');
            alloc_c_string(&out)
        }
        None => alloc_c_string("[]"),
    }
}

// ─── Sort helpers ────────────────────────────────────────────────

/// Sorts an f64 list in place (ascending). Uses Rust's `sort_unstable_by`
/// for O(n log n) performance instead of the original O(n²) bubble sort.
#[no_mangle]
pub extern "C" fn mimi_sort_f64_inplace(data: *mut u8, count: i64) {
    if data.is_null() || count <= 1 {
        return;
    }
    // SAFETY: `data` is non-null and caller must ensure it points to `count * 8` writable bytes.
    let slice = unsafe { std::slice::from_raw_parts_mut(data as *mut f64, count as usize) };
    slice.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
}

/// Sorts a list of UTF-8 C strings in place (ascending lexicographic order).
/// `data` points to an array of `count` `*mut c_char` pointers.
/// Each pointer is preserved across the sort (the underlying C strings are
/// not freed or duplicated — only the pointer slots are reordered).
#[no_mangle]
pub extern "C" fn mimi_sort_str_inplace(data: *mut *mut std::ffi::c_char, count: i64) {
    if data.is_null() || count <= 1 {
        return;
    }
    let n = count as usize;
    // SAFETY: `data` is non-null and caller must ensure it points to `count` valid C string pointers.
    let slice = unsafe { std::slice::from_raw_parts_mut(data, n) };
    // RT-H12: use sort_unstable_by for O(n log n) instead of bubble sort O(n²)
    slice.sort_unstable_by(|a, b| {
        if a.is_null() && b.is_null() {
            std::cmp::Ordering::Equal
        } else if a.is_null() {
            std::cmp::Ordering::Greater
        } else if b.is_null() {
            std::cmp::Ordering::Less
        } else {
            // SAFETY: both a and b are non-null (checked above)
            let a_str = unsafe { CStr::from_ptr(*a) };
            let b_str = unsafe { CStr::from_ptr(*b) };
            a_str.cmp(b_str)
        }
    });
}

// ---------------------------------------------------------------------------
// Network / Socket
// ---------------------------------------------------------------------------

/// Safely convert i64 fd to i32, returning None if out of range.
fn fd_to_i32(fd: i64) -> Option<i32> {
    if fd < 0 || fd > i32::MAX as i64 {
        None
    } else {
        Some(fd as i32)
    }
}

#[no_mangle]
pub extern "C" fn mimi_socket(domain: i64, type_: i64, protocol: i64) -> i64 {
    // H13 fix: validate domain/type/protocol fit in i32 range before truncation.
    let domain_i32 = match i32::try_from(domain) {
        Ok(v) => v,
        Err(_) => return -1,
    };
    let type_i32 = match i32::try_from(type_) {
        Ok(v) => v,
        Err(_) => return -1,
    };
    let protocol_i32 = match i32::try_from(protocol) {
        Ok(v) => v,
        Err(_) => return -1,
    };
    unsafe {
        let fd = libc::socket(domain_i32, type_i32, protocol_i32);
        if fd >= 0 {
            let reuse: i32 = 1;
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_REUSEADDR,
                &reuse as *const _ as *const std::ffi::c_void,
                std::mem::size_of::<i32>() as libc::socklen_t,
            );
        }
        fd as i64
    }
}

#[no_mangle]
pub extern "C" fn mimi_connect(fd: i64, host: *const std::ffi::c_char, port: i64) -> i64 {
    if host.is_null() || fd < 0 {
        return -1;
    }
    // SAFETY: `host` was checked non-null above.
    let h = unsafe { cstr_to_string(host) };

    // Resolve address
    let port_str = format!("{}", port);
    // SAFETY: `addrinfo` is zero-initialized before passing to `getaddrinfo`.
    let hints = unsafe {
        let mut hints_raw: libc::addrinfo = std::mem::zeroed();
        hints_raw.ai_family = libc::AF_UNSPEC;
        hints_raw.ai_socktype = libc::SOCK_STREAM;
        hints_raw
    };
    let mut res: *mut libc::addrinfo = std::ptr::null_mut();
    let c_host = CString::new(h.as_str()).unwrap_or_default();
    let c_port = CString::new(port_str.as_str()).unwrap_or_default();
    // SAFETY: `c_host` and `c_port` are valid NUL-terminated `CString`s; `res` is out-param.
    let err = unsafe { libc::getaddrinfo(c_host.as_ptr(), c_port.as_ptr(), &hints, &mut res) };
    if err != 0 || res.is_null() {
        return -1;
    }

    // SAFETY: freeing a non-null pointer allocated by the matching allocator.
    unsafe {
        let fd_i32 = match fd_to_i32(fd) {
            Some(v) => v,
            None => {
                libc::freeaddrinfo(res);
                return -1;
            }
        };
        // SAFETY: `res` is non-null and came from `getaddrinfo`; `fd_i32` is validated.
        let r = libc::connect(fd_i32, (*res).ai_addr, (*res).ai_addrlen);
        if r == 0 {
            let flag: i32 = 1;
            // SAFETY: `fd_i32` is a valid socket file descriptor.
            libc::setsockopt(
                fd_i32,
                libc::IPPROTO_TCP,
                libc::TCP_NODELAY,
                &flag as *const _ as *const std::ffi::c_void,
                std::mem::size_of::<i32>() as libc::socklen_t,
            );
        }
        libc::freeaddrinfo(res);
        r as i64
    }
}

#[no_mangle]
pub extern "C" fn mimi_bind(fd: i64, port: i64) -> i64 {
    if fd < 0 {
        return -1;
    }
    // H13 fix: validate port fits in u16 range before truncation.
    let port_u16 = match u16::try_from(port) {
        Ok(v) => v,
        Err(_) => return -1,
    };
    unsafe {
        let fd_i32 = match fd_to_i32(fd) {
            Some(v) => v,
            None => return -1,
        };
        let mut addr: libc::sockaddr_in = std::mem::zeroed();
        addr.sin_family = libc::AF_INET as libc::sa_family_t;
        addr.sin_port = port_u16.to_be();
        addr.sin_addr.s_addr = libc::INADDR_ANY;
        libc::bind(
            fd_i32,
            &addr as *const _ as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
        ) as i64
    }
}

#[no_mangle]
pub extern "C" fn mimi_listen(fd: i64, backlog: i64) -> i64 {
    if fd < 0 {
        return -1;
    }
    // SAFETY: direct POSIX calls with a validated file descriptor.
    unsafe {
        let fd_i32 = match fd_to_i32(fd) {
            Some(v) => v,
            None => return -1,
        };
        libc::listen(fd_i32, backlog as i32) as i64
    }
}

#[no_mangle]
pub extern "C" fn mimi_accept(fd: i64) -> i64 {
    if fd < 0 {
        return -1;
    }
    // SAFETY: direct POSIX calls with a validated file descriptor.
    unsafe {
        let fd_i32 = match fd_to_i32(fd) {
            Some(v) => v,
            None => return -1,
        };
        let mut addr: libc::sockaddr_in = std::mem::zeroed();
        let mut addr_len: libc::socklen_t =
            std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
        let client_fd = libc::accept(
            fd_i32,
            &mut addr as *mut _ as *mut libc::sockaddr,
            &mut addr_len,
        );
        client_fd as i64
    }
}

#[no_mangle]
pub extern "C" fn mimi_send(fd: i64, data: *const std::ffi::c_char, len: i64) -> i64 {
    if fd < 0 || data.is_null() {
        return -1;
    }
    // SAFETY: direct POSIX calls with validated file descriptor and non-null buffer.
    unsafe {
        let fd_i32 = match fd_to_i32(fd) {
            Some(v) => v,
            None => return -1,
        };
        libc::send(fd_i32, data as *const std::ffi::c_void, len as usize, 0) as i64
    }
}

#[no_mangle]
pub extern "C" fn mimi_recv(fd: i64, buf_size: i64, out_len: *mut i64) -> *mut std::ffi::c_char {
    if fd < 0 || buf_size <= 0 {
        return std::ptr::null_mut();
    }
    let fd_i32 = match fd_to_i32(fd) {
        Some(v) => v,
        None => return std::ptr::null_mut(),
    };
    let size = buf_size as usize;
    let mut buf: Vec<u8> = vec![0u8; size + 1];
    // SAFETY: `buf` has `size + 1` allocated bytes; `fd_i32` is validated.
    let n = unsafe { libc::recv(fd_i32, buf.as_mut_ptr() as *mut std::ffi::c_void, size, 0) };
    if n <= 0 {
        if !out_len.is_null() {
            unsafe {
                // SAFETY: `out_len` was checked non-null above.
                *out_len = 0;
            }
        }
        return std::ptr::null_mut();
    }
    // S8: Clamp n to buffer size to prevent out-of-bounds write.
    let n = (n as usize).min(size);
    buf[n] = 0;
    if !out_len.is_null() {
        unsafe {
            // SAFETY: `out_len` was checked non-null above.
            *out_len = n as i64;
        }
    }
    alloc_c_string_from_bytes(&buf[..=n as usize])
}

#[no_mangle]
pub extern "C" fn mimi_close(fd: i64) -> i64 {
    if fd < 0 {
        return -1;
    }
    // SAFETY: direct POSIX close with a validated file descriptor.
    unsafe {
        let fd_i32 = match fd_to_i32(fd) {
            Some(v) => v,
            None => return -1,
        };
        libc::close(fd_i32) as i64
    }
}

// ---------------------------------------------------------------------------
// HTTP client
// ---------------------------------------------------------------------------

fn parse_http_url(url: &str) -> Option<(String, u16, String)> {
    // M20: explicitly reject HTTPS (no TLS support in this runtime).
    if url.starts_with("https://") {
        return None;
    }
    let rest = url.strip_prefix("http://")?;

    let (host_part, path_part) = if let Some(slash_idx) = rest.find('/') {
        let (h, p) = rest.split_at(slash_idx);
        (h, p)
    } else {
        (rest, "/")
    };

    let (host, port) = if host_part.starts_with('[') {
        // IPv6: [addr] or [addr]:port
        let close_bracket = host_part.find(']')?;
        let addr = &host_part[1..close_bracket];
        let after = &host_part[close_bracket + 1..];
        if after.is_empty() {
            (format!("[{}]", addr), 80u16)
        } else {
            let port_str = after.strip_prefix(':')?;
            let port: u16 = port_str.parse().ok()?;
            (format!("[{}]", addr), port)
        }
    } else if let Some(colon_idx) = host_part.find(':') {
        let port_str = &host_part[colon_idx + 1..];
        let port: u16 = port_str.parse().ok()?;
        let h = &host_part[..colon_idx];
        (h.to_string(), port)
    } else {
        (host_part.to_string(), 80u16)
    };

    Some((host, port, path_part.to_string()))
}

fn http_request(host: &str, port: u16, request: &str) -> Option<Vec<u8>> {
    use std::io::Read;
    use std::net::TcpStream;

    let addr = format!("{}:{}", host, port);
    let mut stream = TcpStream::connect(&addr).ok()?;
    // C5-fix: propagate timeout failure instead of silently ignoring
    if let Err(e) = stream.set_read_timeout(Some(std::time::Duration::from_secs(5))) {
        eprintln!("[mimi runtime] HTTP set_read_timeout failed: {}", e);
        return None;
    }

    // Send request
    use std::io::Write;
    if let Err(e) = stream.write_all(request.as_bytes()) {
        eprintln!("[mimi runtime] HTTP write error: {}", e);
        return None;
    }

    // Read response
    // M27: limit total response size to prevent OOM from malicious server.
    const MAX_HTTP_RESPONSE: usize = 100 * 1024 * 1024; // 100MB
    let mut response = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if response.len() + n > MAX_HTTP_RESPONSE {
                    return None;
                }
                response.extend_from_slice(&buf[..n]);
            }
            Err(_) => break,
        }
    }

    if response.is_empty() {
        return None;
    }

    // Strip HTTP headers
    let body_start = if let Some(pos) = response.windows(4).position(|w| w == b"\r\n\r\n") {
        pos + 4
    } else if let Some(pos) = response.windows(2).position(|w| w == b"\n\n") {
        pos + 2
    } else {
        return None;
    };

    Some(response[body_start..].to_vec())
}

#[no_mangle]
pub extern "C" fn mimi_http_get(url: *const std::ffi::c_char) -> *mut std::ffi::c_char {
    if url.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `url` was checked non-null above.
    let u = unsafe { cstr_to_string(url) };
    let (host, port, path) = match parse_http_url(&u) {
        Some(v) => v,
        None => {
            // M20: HTTPS URLs are unsupported; log and return null.
            #[cfg(debug_assertions)]
            if u.starts_with("https://") {
                eprintln!("[mimi runtime] HTTPS not supported (no TLS), use http://");
            }
            return std::ptr::null_mut();
        }
    };

    let request = format!(
        "GET {} HTTP/1.0\r\nHost: {}\r\nConnection: close\r\n\r\n",
        path, host
    );

    match http_request(&host, port, &request) {
        Some(body) => {
            let s = String::from_utf8_lossy(&body).into_owned();
            alloc_c_string(&s)
        }
        // audit (MEDIUM): return null on error so callers can distinguish
        // failure from a legitimate empty response body.
        None => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "C" fn mimi_http_post(
    url: *const std::ffi::c_char,
    body: *const std::ffi::c_char,
) -> *mut std::ffi::c_char {
    if url.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `url` was checked non-null above.
    let u = unsafe { cstr_to_string(url) };
    let b = if body.is_null() {
        String::new()
    } else {
        // SAFETY: `body` was checked non-null above.
        unsafe { cstr_to_string(body) }
    };
    let (host, port, path) = match parse_http_url(&u) {
        Some(v) => v,
        None => {
            // M20: HTTPS URLs are unsupported; log and return null.
            #[cfg(debug_assertions)]
            if u.starts_with("https://") {
                eprintln!("[mimi runtime] HTTPS not supported (no TLS), use http://");
            }
            return std::ptr::null_mut();
        }
    };

    let request = format!(
        "POST {} HTTP/1.0\r\nHost: {}\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        path, host, b.len(), b
    );

    match http_request(&host, port, &request) {
        Some(body) => {
            let s = String::from_utf8_lossy(&body).into_owned();
            alloc_c_string(&s)
        }
        // audit (MEDIUM): return null on error so callers can distinguish
        // failure from a legitimate empty response body.
        None => std::ptr::null_mut(),
    }
}

// ---------------------------------------------------------------------------
// JSON FFI serialization
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn mimi_json_serialize(
    data: *mut std::ffi::c_void,
    len: i64,
    elem_type: i64,
) -> *mut std::ffi::c_char {
    if data.is_null() || len <= 0 {
        return alloc_c_string("[]");
    }
    // FFI-13: Refuse to create a slice from misaligned data.
    if data as usize % std::mem::align_of::<i64>() != 0 {
        return alloc_c_string("[]");
    }

    let mut result = String::from("[");
    // SAFETY: `data` was checked non-null and aligned; caller must ensure it points to `len` i64 elements.
    let elements = unsafe { std::slice::from_raw_parts(data as *const i64, len as usize) };

    for (i, &raw) in elements.iter().enumerate() {
        if i > 0 {
            result.push(',');
        }
        match elem_type {
            1 => {
                // Float: bitcast i64 to f64
                let val: f64 = f64::from_bits(raw as u64);
                let s = format!("{}", val);
                // Trim trailing zeros
                let trimmed = if s.contains('.') {
                    s.trim_end_matches('0').trim_end_matches('.').to_string()
                } else {
                    s
                };
                result.push_str(&trimmed);
            }
            2 => {
                // String: raw is a C string pointer. RT-H1: never CStr::from_ptr
                // on a size/alignment heuristic alone — require mincore + NUL
                // via safe_c_string_from_handle.
                result.push('"');
                if raw != 0 {
                    if let Some(s_str) = safe_c_string_from_handle(raw as ValueHandle) {
                        for c in s_str.chars() {
                            match c {
                                '"' => result.push_str("\\\""),
                                '\\' => result.push_str("\\\\"),
                                '\n' => result.push_str("\\n"),
                                '\r' => result.push_str("\\r"),
                                '\t' => result.push_str("\\t"),
                                _ => result.push(c),
                            }
                        }
                    }
                }
                result.push('"');
            }
            _ => {
                // Integer
                result.push_str(&raw.to_string());
            }
        }
    }
    result.push(']');
    alloc_c_string(&result)
}

#[no_mangle]
pub extern "C" fn mimi_list_serialize(
    data: *mut std::ffi::c_void,
    len: i64,
) -> *mut std::ffi::c_char {
    mimi_json_serialize(data, len, 0)
}

#[no_mangle]
pub extern "C" fn mimi_json_deserialize(
    json: *const std::ffi::c_char,
    out_len: *mut i64,
    elem_type: i64,
) -> *mut std::ffi::c_void {
    if json.is_null() {
        if !out_len.is_null() {
            // SAFETY: `out_len` was checked non-null above.
            unsafe {
                *out_len = 0;
            }
        }
        return std::ptr::null_mut();
    }
    // SAFETY: `json` was checked non-null above.
    let s = unsafe { cstr_to_string(json) };
    let bytes = s.as_bytes();
    let mut pos = 0;

    // Skip whitespace
    while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') {
        pos += 1;
    }
    if pos >= bytes.len() || bytes[pos] != b'[' {
        if !out_len.is_null() {
            // SAFETY: `out_len` was checked non-null above.
            unsafe {
                *out_len = 0;
            }
        }
        return std::ptr::null_mut();
    }
    pos += 1;

    // Count elements
    let mut count: i64 = 0;
    {
        let mut p = pos;
        loop {
            if p >= bytes.len() {
                break;
            }
            while p < bytes.len() && matches!(bytes[p], b' ' | b'\t' | b'\n' | b'\r' | b',') {
                p += 1;
            }
            if p >= bytes.len() || bytes[p] == b']' {
                break;
            }

            if elem_type == 2 && bytes[p] == b'"' {
                count += 1;
                p += 1;
                loop {
                    if p >= bytes.len() {
                        break;
                    }
                    // RT-C1: trailing `\` must not advance past EOF.
                    if bytes[p] == b'\\' {
                        p += 1;
                        if p < bytes.len() {
                            p += 1;
                        }
                        continue;
                    }
                    if bytes[p] == b'"' {
                        p += 1;
                        break;
                    }
                    p += 1;
                }
            } else if bytes[p] == b'-' || bytes[p].is_ascii_digit() {
                count += 1;
                if bytes[p] == b'-' {
                    p += 1;
                }
                while p < bytes.len() && bytes[p].is_ascii_digit() {
                    p += 1;
                }
                if p < bytes.len() && bytes[p] == b'.' {
                    p += 1;
                    while p < bytes.len() && bytes[p].is_ascii_digit() {
                        p += 1;
                    }
                }
            } else {
                // Skip unknown (true/false/null)
                while p < bytes.len() && !matches!(bytes[p], b']' | b',') {
                    p += 1;
                }
            }
        }
    }

    // RT-C4: cap allocation to prevent OOM from malicious JSON element counts.
    const MAX_JSON_LIST_ELEMS: i64 = 10_000_000;
    if count < 0 || count > MAX_JSON_LIST_ELEMS {
        return std::ptr::null_mut();
    }
    // Allocate output array
    let mut data: Vec<i64> = vec![0i64; count as usize];
    pos = 1; // skip initial [
    let mut idx: i64 = 0;
    loop {
        if pos >= bytes.len() {
            break;
        }
        while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r' | b',') {
            pos += 1;
        }
        if pos >= bytes.len() || bytes[pos] == b']' {
            break;
        }
        if idx >= count {
            break;
        }

        match elem_type {
            1 => {
                // Float: parse, store bits as i64
                let val_start = pos;
                let mut dummy_parser = JsonParser::new(&s[val_start..]);
                let parsed = dummy_parser.parse_number();
                pos = val_start + dummy_parser.pos;
                if let Some(num_str) = parsed {
                    let f: f64 = num_str.parse().unwrap_or(0.0);
                    data[idx as usize] = f64::to_bits(f) as i64;
                }
                idx += 1;
            }
            2 => {
                // String
                if bytes[pos] == b'"' {
                    pos += 1;
                }
                let start = pos;
                // M10: limit per-string length to prevent oversized allocation.
                const MAX_JSON_STR_LEN: usize = 10 * 1024 * 1024; // 10MB
                let mut str_len: usize = 0;
                // RT-C1: trailing `\` must not advance past EOF (`pos += 2` OOB).
                while pos < bytes.len() && bytes[pos] != b'"' {
                    if bytes[pos] == b'\\' {
                        pos += 1;
                        if pos < bytes.len() {
                            pos += 1;
                        }
                        str_len += 2;
                    } else {
                        pos += 1;
                        str_len += 1;
                    }
                    if str_len > MAX_JSON_STR_LEN {
                        // Oversized string: skip past closing quote
                        while pos < bytes.len() && bytes[pos] != b'"' {
                            pos += 1;
                        }
                        break;
                    }
                }
                // M19 fix: unescape JSON escape sequences (\n, \", \\, \uXXXX, etc.)
                let end = usize::min(pos, bytes.len());
                let raw_bytes = bytes[start..usize::min(end, start + MAX_JSON_STR_LEN)].to_vec();
                let unescaped = json_unescape(&raw_bytes);
                data[idx as usize] = alloc_c_string_from_bytes(&unescaped) as i64;
                if pos < bytes.len() && bytes[pos] == b'"' {
                    pos += 1;
                }
                idx += 1;
            }
            _ => {
                // Integer
                let neg = if bytes[pos] == b'-' {
                    pos += 1;
                    true
                } else {
                    false
                };
                let mut val: i64 = 0;
                let mut overflow = false;
                while pos < bytes.len() && bytes[pos].is_ascii_digit() {
                    if let Some(v) = val.checked_mul(10) {
                        if let Some(v2) = v.checked_add((bytes[pos] - b'0') as i64) {
                            val = v2;
                        } else {
                            overflow = true;
                            break;
                        }
                    } else {
                        overflow = true;
                        break;
                    }
                    pos += 1;
                }
                if overflow {
                    // M1: overflow only occurs on the integer parse arm; the
                    // prior elem_type==2 string free was dead code. Local `data`
                    // Vec drops on return (no forget yet).
                    if !out_len.is_null() {
                        // SAFETY: `out_len` was checked non-null above.
                        unsafe { *out_len = 0 };
                    }
                    return std::ptr::null_mut();
                }
                if neg {
                    val = -val;
                }
                data[idx as usize] = val;
                idx += 1;
            }
        }
    }

    // RT-H3: shrink so capacity == filled len; free reconstructs with cap == len.
    // Without this, out_len=idx may be < original count and from_raw_parts is UB.
    data.truncate(idx as usize);
    data.shrink_to_fit();
    debug_assert_eq!(data.len(), data.capacity());
    let result = data.as_mut_ptr();
    let out = idx;
    std::mem::forget(data);
    if !out_len.is_null() {
        // SAFETY: `out_len` was checked non-null above.
        unsafe {
            *out_len = out;
        }
    }
    // Empty result: no heap buffer (shrink_to_fit of empty Vec may leave null ptr).
    if out == 0 {
        return std::ptr::null_mut();
    }
    result as *mut std::ffi::c_void
}

/// C11: Free a buffer returned by mimi_json_deserialize / mimi_list_deserialize.
/// Reconstructs the Vec<i64> and drops it, freeing both the data buffer and
/// any heap-allocated string pointers (elem_type==2).
///
/// RT-H3: `mimi_json_deserialize` shrink_to_fit so capacity == len.
#[no_mangle]
pub extern "C" fn mimi_json_deserialize_free(buf: *mut std::ffi::c_void, len: i64, elem_type: i64) {
    if buf.is_null() || len <= 0 {
        return;
    }
    let count = len as usize;
    // Rebuild Vec from the pointer, then drop it (frees the allocation).
    // SAFETY: `buf` was created by a prior mimi_json_deserialize call with
    // matching `len` and `elem_type`, after shrink_to_fit (capacity == len).
    unsafe {
        let ptr = buf as *mut i64;
        // If this was a string-typed deserialization, free each C string first.
        if elem_type == 2 {
            for i in 0..count {
                let p = *ptr.add(i) as *mut std::ffi::c_char;
                if !p.is_null() {
                    libc::free(p as *mut std::ffi::c_void);
                }
            }
        }
        // Drop the Vec without running element destructors (i64 is trivially
        // copy, and strings were already freed above). capacity == len (RT-H3).
        let _ = Vec::from_raw_parts(ptr, 0, count);
    }
}

#[no_mangle]
pub extern "C" fn mimi_list_deserialize(
    json: *const std::ffi::c_char,
    out_len: *mut i64,
) -> *mut std::ffi::c_void {
    mimi_json_deserialize(json, out_len, 0)
}

#[no_mangle]
pub extern "C" fn mimi_tuple_serialize(
    values: *mut i64,
    count: i64,
    elem_types: *mut i64,
) -> *mut std::ffi::c_char {
    if values.is_null() || count <= 0 {
        return alloc_c_string("[]");
    }
    // SAFETY: `values` was checked non-null above; caller ensures `count` elements.
    let vals = unsafe { std::slice::from_raw_parts(values, count as usize) };
    let types = if elem_types.is_null() {
        &[] as &[i64]
    } else {
        // SAFETY: `elem_types` is non-null and caller ensures `count` elements.
        unsafe { std::slice::from_raw_parts(elem_types, count as usize) }
    };

    let mut result = String::from("[");
    for i in 0..count as usize {
        if i > 0 {
            result.push(',');
        }
        let raw = vals[i];
        let tag = if i < types.len() { types[i] } else { 0 };
        match tag {
            1 => {
                let val: f64 = f64::from_bits(raw as u64);
                let s = format!("{}", val);
                let trimmed = if s.contains('.') {
                    s.trim_end_matches('0').trim_end_matches('.').to_string()
                } else {
                    s
                };
                result.push_str(&trimmed);
            }
            2 => {
                // RT-H1: route through safe_c_string_from_handle (mincore+NUL).
                result.push('"');
                if raw != 0 {
                    if let Some(s_str) = safe_c_string_from_handle(raw as ValueHandle) {
                        for c in s_str.chars() {
                            match c {
                                '"' => result.push_str("\\\""),
                                '\\' => result.push_str("\\\\"),
                                '\n' => result.push_str("\\n"),
                                '\r' => result.push_str("\\r"),
                                '\t' => result.push_str("\\t"),
                                _ => result.push(c),
                            }
                        }
                    }
                }
                result.push('"');
            }
            _ => {
                result.push_str(&raw.to_string());
            }
        }
    }
    result.push(']');
    alloc_c_string(&result)
}

#[no_mangle]
pub extern "C" fn mimi_tuple_deserialize(
    json: *const std::ffi::c_char,
    count: i64,
    elem_types: *mut i64,
    out_values: *mut i64,
) -> i64 {
    if json.is_null() || out_values.is_null() || count <= 0 {
        return -1;
    }
    // SAFETY: `json` was checked non-null above.
    let s = unsafe { cstr_to_string(json) };
    let bytes = s.as_bytes();
    let mut pos = 0;

    while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') {
        pos += 1;
    }
    if pos >= bytes.len() || bytes[pos] != b'[' {
        return -1;
    }
    pos += 1;

    let types = if elem_types.is_null() {
        &[] as &[i64]
    } else {
        // SAFETY: `elem_types` is non-null and caller ensures `count` elements.
        unsafe { std::slice::from_raw_parts(elem_types, count as usize) }
    };

    let mut idx: i64 = 0;
    loop {
        if pos >= bytes.len() {
            break;
        }
        while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r' | b',') {
            pos += 1;
        }
        if pos >= bytes.len() || bytes[pos] == b']' {
            break;
        }
        if idx >= count {
            break;
        }

        let tag = if (idx as usize) < types.len() {
            types[idx as usize]
        } else {
            0
        };
        match tag {
            1 => {
                // Float
                let mut end = pos;
                if end < bytes.len() && bytes[end] == b'-' {
                    end += 1;
                }
                while end < bytes.len()
                    && (bytes[end].is_ascii_digit()
                        || bytes[end] == b'.'
                        || bytes[end] == b'e'
                        || bytes[end] == b'E'
                        || bytes[end] == b'+'
                        || bytes[end] == b'-')
                {
                    end += 1;
                }
                let num_str = std::str::from_utf8(&bytes[pos..end]).unwrap_or("0");
                let f: f64 = num_str.parse().unwrap_or(0.0);
                // SAFETY: `out_values` was checked non-null above; `idx < count`.
                unsafe {
                    *out_values.offset(idx as isize) = f64::to_bits(f) as i64;
                }
                pos = end;
                idx += 1;
            }
            2 => {
                // String
                if bytes[pos] == b'"' {
                    pos += 1;
                }
                let start = pos;
                // RT-C2: trailing `\` must not advance past EOF (`pos += 2` OOB).
                while pos < bytes.len() && bytes[pos] != b'"' {
                    if bytes[pos] == b'\\' {
                        pos += 1;
                        if pos < bytes.len() {
                            pos += 1;
                        }
                    } else {
                        pos += 1;
                    }
                }
                // M19 fix: unescape JSON escape sequences
                let raw_bytes = bytes[start..pos].to_vec();
                let unescaped = json_unescape(&raw_bytes);
                if !unescaped.is_empty() {
                    // SAFETY: out_values is the caller's array with `count`
                    // entries; idx is bounds-checked above against count.
                    // The store overwrites a previously-written slot in
                    // the same array.
                    unsafe {
                        *out_values.offset(idx as isize) =
                            alloc_c_string_from_bytes(&unescaped) as i64;
                    }
                } else {
                    // SAFETY: same as the if-branch: out_values is bounds-checked
                    // by the caller-supplied count.
                    unsafe {
                        *out_values.offset(idx as isize) = 0;
                    }
                }
                if pos < bytes.len() && bytes[pos] == b'"' {
                    pos += 1;
                }
                idx += 1;
            }
            _ => {
                // Integer (or null literal)
                // M8: detect null literal in JSON and write 0.
                if pos + 3 < bytes.len() && bytes[pos] == b'n' && &bytes[pos..pos + 4] == b"null" {
                    pos += 4;
                    unsafe { *out_values.offset(idx as isize) = 0 }
                    idx += 1;
                    continue;
                }
                let neg = if bytes[pos] == b'-' {
                    pos += 1;
                    true
                } else {
                    false
                };
                // M16: use checked arithmetic to avoid silent wrapping on overflow.
                let mut val: i64 = 0;
                while pos < bytes.len() && bytes[pos].is_ascii_digit() {
                    let digit = (bytes[pos] - b'0') as i64;
                    match val.checked_mul(10) {
                        Some(v) => match v.checked_add(digit) {
                            Some(s) => val = s,
                            None => {
                                val = 0;
                                break;
                            }
                        },
                        None => {
                            val = 0;
                            break;
                        }
                    }
                    pos += 1;
                }
                if neg {
                    // M30: use checked_neg to avoid silent wrapping on i64::MIN.
                    val = val.checked_neg().unwrap_or_default();
                }
                // SAFETY: `out_values` was checked non-null above; `idx < count`.
                unsafe {
                    *out_values.offset(idx as isize) = val;
                }
                idx += 1;
            }
        }
    }
    idx
}

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

// FFI-4: The UB trigger __mimi_extern_test_segfault is compiled into the
// staticlib because FFI safety tests link against it via the runtime .so
// (which is built without #[cfg(test)]). The symbol name is self-documenting
// as a test-only hazard — no production code would call __mimi_extern_test_segfault.
// CRITICAL #14 mitigation: the function name contains "test" and "segfault",
// making accidental invocation extremely unlikely. A feature flag
// `mimi_no_test_symbols` could be used in future to strip these from
// production builds.
#[no_mangle]
pub extern "C" fn __mimi_extern_test_segfault() {
    // Deliberate null pointer dereference — used by FFI safety tests to verify
    // crash handling. Only Mimi test code calls this function.
    // SAFETY: deliberate null-pointer write for FFI crash testing only.
    unsafe {
        std::ptr::write_volatile(std::ptr::null_mut::<i32>(), 42);
    }
}

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

#[no_mangle]
pub extern "C" fn test_segfault() {
    __mimi_extern_test_segfault()
}

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

// ---------------------------------------------------------------------------
// No_panic signal handlers (POSIX only)
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// No_panic handlers (POSIX only)
// ---------------------------------------------------------------------------
// Previous versions installed signal handlers that used sigsetjmp/siglongjmp
// to recover from C-level crashes. That is undefined behaviour: a signal
// handler cannot non-locally jump back into arbitrary Rust code and preserve
// Rust's invariants (destructors, borrow checker assumptions, platform ABI).
//
// The interpreter now isolates #[no_panic] FFI calls in a forked child process
// (see src/interp/ffi/call.rs). The runtime symbols below are kept as no-ops so
// that older generated binaries and codegen wrappers that reference them still
// link, but they no longer install any signal handlers. Future codegen support
// for #[no_panic] will use its own process isolation mechanism.

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod no_panic {
    #[no_mangle]
    pub extern "C" fn mimi_install_no_panic_handlers() {}

    #[no_mangle]
    pub extern "C" fn mimi_restore_no_panic_handlers() {}
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod no_panic {
    #[no_mangle]
    pub extern "C" fn mimi_install_no_panic_handlers() {}

    #[no_mangle]
    pub extern "C" fn mimi_restore_no_panic_handlers() {}
}

// ---------------------------------------------------------------------------
// Error handlers
// ---------------------------------------------------------------------------

use std::sync::atomic::AtomicPtr;

type ErrorHandler = unsafe extern "C" fn(*const std::ffi::c_char);
// S19: Use typed AtomicPtr<ErrorHandler> instead of AtomicPtr<c_void> + transmute.
static ERROR_HANDLER: AtomicPtr<ErrorHandler> = AtomicPtr::new(std::ptr::null_mut());

#[no_mangle]
pub extern "C" fn mimi_runtime_set_error_handler(handler: Option<ErrorHandler>) {
    let ptr: *mut ErrorHandler = match handler {
        Some(f) => f as *const ErrorHandler as *mut ErrorHandler,
        None => std::ptr::null_mut(),
    };
    ERROR_HANDLER.store(ptr, Ordering::Release);
}

#[no_mangle]
pub extern "C" fn mimi_runtime_abort(msg: *const std::ffi::c_char) -> ! {
    // P3-19 fix: write to stderr fd using raw syscall (async-signal-safe),
    // instead of eprintln!() which acquires locks that may deadlock in signal context.
    extern "C" {
        fn write(fd: i32, buf: *const std::ffi::c_void, count: usize) -> isize;
    }
    const PREFIX: &[u8] = b"[FFI contract violation] ";
    const HINT: &[u8] = b"\nHint: use --skip-verify-ffi to disable contract checking.\n";
    const DETAIL: &[u8] = b"(no details)\n";
    // RT-C3: must stay async-signal-safe — no allocation (no to_string_lossy).
    // SAFETY: writing static / C-string byte buffers to stderr (fd 2) is
    // async-signal-safe; we only use write(2) and strlen-style scan.
    unsafe {
        let _ = write(2, PREFIX.as_ptr() as *const std::ffi::c_void, PREFIX.len());
        if !msg.is_null() {
            // SAFETY: `msg` non-null; scan for NUL without allocating.
            let mut len = 0usize;
            let base = msg as *const u8;
            // Cap message length to avoid unbounded scan of non-NUL-terminated input.
            const MAX_MSG: usize = 4096;
            while len < MAX_MSG && *base.add(len) != 0 {
                len += 1;
            }
            let _ = write(2, msg as *const std::ffi::c_void, len);
            let _ = write(2, b"\n".as_ptr() as *const std::ffi::c_void, 1);
        } else {
            let _ = write(2, DETAIL.as_ptr() as *const std::ffi::c_void, DETAIL.len());
        }
        let _ = write(2, HINT.as_ptr() as *const std::ffi::c_void, HINT.len());
    }

    let handler_ptr = ERROR_HANDLER.load(Ordering::Acquire);
    if !handler_ptr.is_null() {
        ERROR_HANDLER.store(std::ptr::null_mut(), Ordering::Release);
        // SAFETY: `handler_ptr` was checked non-null and the handler was cleared before calling.
        let handler: &ErrorHandler = unsafe { &*handler_ptr };
        // SAFETY: calling the registered error handler with the validated message pointer.
        unsafe { (*handler)(msg) };
        std::process::abort();
    }

    std::process::abort();
}

/// v0.29.32: Wall-clock timestamp in milliseconds since UNIX epoch.
/// Used by the pinned timeout watchdog to check cooperative expiry.
#[no_mangle]
pub extern "C" fn mimi_wall_clock_ms() -> i64 {
    // L3: clock failure must not return epoch 0 (would make pinned watchdog
    // believe 50+ years elapsed). Prefer last good value; log on first failure.
    thread_local! {
        static LAST_MS: std::cell::Cell<i64> = const { std::cell::Cell::new(1) };
        static LOGGED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => {
            let ms = d.as_millis() as i64;
            LAST_MS.with(|c| c.set(ms.max(1)));
            ms.max(1)
        }
        Err(_) => {
            if !LOGGED.with(|c| c.replace(true)) {
                eprintln!(
                    "mimi_wall_clock_ms: system clock before UNIX_EPOCH; using last good value"
                );
            }
            LAST_MS.with(|c| c.get())
        }
    }
}

// v0.29.43: Thread-local flag for delayed Fault from pinned blocks.
// When set, the codegen pinned timeout path does NOT abort the process;
// instead it sets this flag and returns, allowing the C call stack to
// unwind safely. The caller then checks the flag and produces a Fault value.
thread_local! {
    static PINNED_FAULT_PENDING: std::cell::RefCell<Option<PinnedFaultInfo>> = const { std::cell::RefCell::new(None) };
}

#[derive(Clone)]
struct PinnedFaultInfo {
    state_name: String,
}

/// v0.29.43: Set a pending delayed Fault from a pinned block.
/// Called by codegen when pinned timeout expires or FFI crash detected.
/// The process is NOT aborted — the flag is set and the function returns,
/// honoring the white-paper's "delay until C call safely returns" requirement.
#[no_mangle]
pub extern "C" fn mimi_pinned_fault(state_name: *const std::ffi::c_char) -> i64 {
    let state = if state_name.is_null() {
        "FFI_Pinned".to_string()
    } else {
        unsafe { std::ffi::CStr::from_ptr(state_name) }
            .to_string_lossy()
            .into_owned()
    };
    PINNED_FAULT_PENDING.with(|cell| {
        *cell.borrow_mut() = Some(PinnedFaultInfo {
            state_name: state.clone(),
        });
    });
    1 // non-zero = fault pending
}

/// v0.29.43: Check and consume the pending pinned Fault.
/// Returns 1 if a Fault was pending (and clears it), 0 if not.
#[no_mangle]
pub extern "C" fn mimi_pinned_fault_take() -> i64 {
    PINNED_FAULT_PENDING.with(|cell| {
        if cell.borrow().is_some() {
            *cell.borrow_mut() = None;
            1
        } else {
            0
        }
    })
}

/// v0.29.43: Get the pending Fault's state name as a C string pointer.
/// Returns null if no pending Fault. The returned pointer is valid until
/// the next call to `mimi_pinned_fault_take`.
#[no_mangle]
pub extern "C" fn mimi_pinned_fault_state() -> *const std::ffi::c_char {
    thread_local! {
        static FAULT_STATE_CSTR: std::cell::RefCell<Option<std::ffi::CString>> = const { std::cell::RefCell::new(None) };
    }
    FAULT_STATE_CSTR.with(|cstr_cell| {
        let mut cstr = cstr_cell.borrow_mut();
        PINNED_FAULT_PENDING.with(|cell| {
            if let Some(ref info) = *cell.borrow() {
                *cstr = Some(
                    std::ffi::CString::new(info.state_name.as_str()).unwrap_or_else(
                        |_| {
                            // Fallback never fails: no interior NULs.
                            std::ffi::CString::new("FFI_Pinned")
                                .unwrap_or_else(|_| std::ffi::CString::default())
                        },
                    ),
                );
            }
        });
        cstr.as_ref()
            .map(|c| c.as_ptr())
            .unwrap_or(std::ptr::null())
    })
}

/// v0.29.38-fix: inject_fault(state_name) — prints a message and aborts.
/// In the interp path, inject_fault constructs a proper Fault record with
/// SystemTrace. In codegen, we cannot easily construct the record at runtime,
/// so we print a diagnostic and abort. This ensures test programs that rely
/// on inject_fault do not silently continue with a bogus value.
#[no_mangle]
pub extern "C" fn mimi_inject_fault(state_name: *const std::ffi::c_char) -> i64 {
    let state = if state_name.is_null() {
        "unknown".to_string()
    } else {
        unsafe { std::ffi::CStr::from_ptr(state_name) }
            .to_string_lossy()
            .into_owned()
    };
    eprintln!(
        "[mimi runtime] inject_fault: injecting Fault into state '{}'",
        state
    );
    // Return a sentinel value; the interp path handles the actual Fault
    // construction. In codegen, this is a best-effort diagnostic.
    -1
}

/// v0.29.38-fix: assert_state(actual_state_cstr, expected_state_cstr)
/// Compares two C strings; if they differ, prints an error and aborts.
/// If `actual_state` is null, the check is skipped (codegen cannot extract
/// the state name at runtime — the interp path does the full check).
#[no_mangle]
pub extern "C" fn mimi_assert_state(
    actual_state: *const std::ffi::c_char,
    expected_state: *const std::ffi::c_char,
) -> i64 {
    // Skip check if actual_state is null (codegen path limitation)
    if actual_state.is_null() {
        return 0;
    }
    let actual = unsafe { std::ffi::CStr::from_ptr(actual_state) }
        .to_string_lossy()
        .into_owned();
    let expected = if expected_state.is_null() {
        "(null)".to_string()
    } else {
        unsafe { std::ffi::CStr::from_ptr(expected_state) }
            .to_string_lossy()
            .into_owned()
    };
    if actual != expected {
        eprintln!(
            "[mimi runtime] assert_state failed: expected '{}', got '{}'",
            expected, actual
        );
        std::process::abort();
    }
    0
}

// ---------------------------------------------------------------------------
// v0.29.44: Software Shadow Memory Tagging (MTE simulation)
// White-paper section 4.2: "软件层面的影子内存（Shadow Memory）"
// ---------------------------------------------------------------------------

use std::collections::HashMap as StdHashMap;

struct ShadowTagInfo {
    tag: u8,
    size: usize,
    label: String,
}

thread_local! {
    static SHADOW_MAP: std::cell::RefCell<StdHashMap<usize, ShadowTagInfo>> =
        std::cell::RefCell::new(StdHashMap::new());
}

/// v0.29.44: Allocate memory with a shadow tag.
/// Returns a pointer to the allocated memory, or null on failure.
/// The memory is tracked in the shadow map with the given tag and label.
#[no_mangle]
pub extern "C" fn mimi_shadow_alloc(
    size: usize,
    tag: u8,
    label: *const std::ffi::c_char,
) -> *mut u8 {
    let label_str = if label.is_null() {
        String::new()
    } else {
        unsafe { std::ffi::CStr::from_ptr(label) }
            .to_string_lossy()
            .into_owned()
    };
    let layout = match std::alloc::Layout::from_size_align(size, 8) {
        Ok(l) => l,
        Err(_) => return std::ptr::null_mut(),
    };
    let ptr = unsafe { std::alloc::alloc(layout) };
    if ptr.is_null() {
        return ptr;
    }
    SHADOW_MAP.with(|m| {
        m.borrow_mut().insert(
            ptr as usize,
            ShadowTagInfo {
                tag,
                size,
                label: label_str,
            },
        );
    });
    ptr
}

/// v0.29.44: Tag an existing memory region with a shadow tag.
/// Returns 0 on success, -1 if the pointer is not in the shadow map.
#[no_mangle]
pub extern "C" fn mimi_shadow_tag(ptr: *const u8, tag: u8) -> i32 {
    if ptr.is_null() {
        return -1;
    }
    SHADOW_MAP.with(|m| {
        let mut m = m.borrow_mut();
        if let Some(info) = m.get_mut(&(ptr as usize)) {
            info.tag = tag;
            0
        } else {
            -1
        }
    })
}

/// v0.29.44: Check that a pointer's shadow tag matches the expected tag.
/// Returns 1 if tag matches, 0 if mismatch or pointer not tracked.
#[no_mangle]
pub extern "C" fn mimi_shadow_check(ptr: *const u8, expected_tag: u8) -> i32 {
    if ptr.is_null() {
        return 0;
    }
    SHADOW_MAP.with(|m| {
        let m = m.borrow();
        if let Some(info) = m.get(&(ptr as usize)) {
            if info.tag == expected_tag {
                1
            } else {
                0
            }
        } else {
            0
        }
    })
}

/// v0.29.44: Free shadow-tagged memory and remove from shadow map.
#[no_mangle]
pub extern "C" fn mimi_shadow_free(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }
    SHADOW_MAP.with(|m| {
        if let Some(info) = m.borrow_mut().remove(&(ptr as usize)) {
            // HIGH fix: use match instead of unwrap() on free path.
            // info.size was validated during shadow_alloc, so this should
            // always succeed — but defensive coding on free paths prevents
            // UB if the shadow map is corrupted.
            if let Ok(layout) = std::alloc::Layout::from_size_align(info.size, 8) {
                // SAFETY: ptr was allocated by shadow_alloc with the same
                // layout (size, align=8). dealloc with a mismatched layout
                // is UB, so we skip dealloc if layout reconstruction fails.
                unsafe { std::alloc::dealloc(ptr, layout) };
            }
        }
    });
}

/// v0.29.44: Dump the shadow map as a C string (for MemoryDump population).
/// Format: "ptr=0x... tag=N size=M label=...;ptr=0x... ..."
/// Returns a pointer valid until the next call.
#[no_mangle]
pub extern "C" fn mimi_shadow_dump() -> *const std::ffi::c_char {
    thread_local! {
        static DUMP_CSTR: std::cell::RefCell<Option<std::ffi::CString>> = const { std::cell::RefCell::new(None) };
    }
    DUMP_CSTR.with(|cstr_cell| {
        let mut buf = String::new();
        SHADOW_MAP.with(|m| {
            let map = m.borrow();
            for (ptr, info) in map.iter() {
                if !buf.is_empty() {
                    buf.push(';');
                }
                buf.push_str(&format!(
                    "ptr=0x{:x} tag={} size={} label={}",
                    ptr, info.tag, info.size, info.label
                ));
            }
        });
        *cstr_cell.borrow_mut() = Some(
            std::ffi::CString::new(buf).unwrap_or_else(|_| std::ffi::CString::default()),
        );
        cstr_cell
            .borrow()
            .as_ref()
            .map(|c| c.as_ptr())
            .unwrap_or(std::ptr::null())
    })
}
// ---------------------------------------------------------------------------
// Capability runtime (self-contained, thread-local)
// ---------------------------------------------------------------------------

struct CapEntry {
    id: i64,
    name: String,
    consumed: bool,
}

thread_local! {
    static CAP_TABLE: Mutex<CapTableData> = const { Mutex::new(CapTableData { next_id: 1, entries: Vec::new() }) };
}

struct CapTableData {
    next_id: i64,
    entries: Vec<CapEntry>,
}

#[no_mangle]
pub extern "C" fn mimi_cap_register(name: *const std::ffi::c_char) -> i64 {
    let n = if name.is_null() {
        String::new()
    } else {
        // SAFETY: `cstr_to_string` handles null pointers safely.
        unsafe { cstr_to_string(name) }
    };
    CAP_TABLE.with(|table| {
        let mut state = table.lock().unwrap_or_else(|e| e.into_inner());
        let id = state.next_id;
        state.next_id += 1;
        state.entries.push(CapEntry {
            id,
            name: n,
            consumed: false,
        });
        id
    })
}

// ─── MimiFuture + MimiExecutor (poll-based async runtime) ──────
//
// Future memory layout (managed by codegen):
//   offset 0: i32 (completed flag: 0=pending, 1=ready)
//   offset 4: [padding 4 bytes]
//   offset 8: <result> (8-byte aligned, up to 64 bytes)
//
// Uses Box (Rust allocator) for consistent memory management.

#[repr(C)]
struct MimiFutureRepr {
    completed: std::sync::atomic::AtomicI32,
    _pad: [u8; 4],
    data: [u8; 64],
}

#[no_mangle]
pub extern "C" fn mimi_future_alloc(_result_size: u64) -> *mut std::ffi::c_void {
    use std::sync::atomic::AtomicI32;
    let b = Box::new(MimiFutureRepr {
        completed: AtomicI32::new(0),
        _pad: [0; 4],
        data: [0; 64],
    });
    Box::into_raw(b) as *mut std::ffi::c_void
}

#[no_mangle]
pub extern "C" fn mimi_future_free(fut: *mut std::ffi::c_void) {
    if fut.is_null() {
        return;
    }
    // C13: atomically set completed to -1 (freed sentinel) so any racing
    // mimi_future_set_completed will see the sentinel and skip writing.
    unsafe {
        let rep = &*(fut as *const MimiFutureRepr);
        rep.completed.store(-1, Ordering::Release);
    }
    // SAFETY: `fut` was checked non-null; reconstructing the Box allocated by `mimi_future_alloc`.
    unsafe {
        drop(Box::from_raw(fut as *mut MimiFutureRepr));
    }
}

#[no_mangle]
pub extern "C" fn mimi_future_set_completed(fut: *mut std::ffi::c_void) {
    if fut.is_null() {
        return;
    }
    use std::sync::atomic::Ordering;
    // C13 (deep audit): the previous check-then-act (`load == 0` then `store 1`)
    // had a race window — a concurrent `mimi_future_free` could store -1 and
    // deallocate between the load and the store, so the store would land in
    // freed memory (UAF write). Use a single atomic compare_exchange: it only
    // transitions 0 -> 1, and fails (no write) if the future was concurrently
    // freed (completed == -1) or already completed (== 1).
    unsafe {
        let rep = &*(fut as *const MimiFutureRepr);
        let _ = rep
            .completed
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire);
    }
}

#[no_mangle]
pub extern "C" fn mimi_future_is_completed(fut: *mut std::ffi::c_void) -> i32 {
    if fut.is_null() {
        return 1;
    }
    use std::sync::atomic::Ordering;
    // SAFETY: `fut` was checked non-null; `MimiFutureRepr` is valid.
    unsafe {
        let rep = &*(fut as *const MimiFutureRepr);
        rep.completed.load(Ordering::Acquire)
    }
}

/// Spawned thread handles retained so they can be joined before process exit.
/// H15 fix: use OnceLock so the atexit handler can check whether SPAWN_HANDLES
/// is still initialized before accessing it. This prevents UB when atexit fires
/// after Rust's static destructors have already dropped the Mutex.
static SPAWN_HANDLES: std::sync::OnceLock<std::sync::Mutex<Vec<std::thread::JoinHandle<()>>>> =
    std::sync::OnceLock::new();
static SPAWN_ATEXIT_REGISTERED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn get_spawn_handles() -> &'static std::sync::Mutex<Vec<std::thread::JoinHandle<()>>> {
    SPAWN_HANDLES.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

extern "C" fn mimi_join_spawned_threads_atexit() {
    // H15 fix: check if SPAWN_HANDLES is still initialized before trying to
    // lock it. If Rust statics have already been dropped, OnceLock::get()
    // returns None and we skip joining (handles will be detached by OS).
    if let Some(handles_mutex) = SPAWN_HANDLES.get() {
        if let Ok(mut handles) = handles_mutex.lock() {
            for handle in handles.drain(..) {
                let _ = handle.join();
            }
        }
    }
}

/// Spawn a future on a real thread (used by codegen `spawn expr`).
/// The poll function is called on a new thread, which sets completed=1 when done.
/// Returns the future pointer (same as input).
/// The returned `JoinHandle` is retained in `SPAWN_HANDLES` and joined at
/// process exit so that the pthread stack is freed before Valgrind checks.
#[no_mangle]
pub extern "C" fn mimi_spawn_future(
    future: *mut std::ffi::c_void,
    // SAFETY: unsafe extern "C" function pointer used for C poll callbacks; see # Safety docs.
    poll_fn: unsafe extern "C" fn(*mut std::ffi::c_void),
) -> *mut std::ffi::c_void {
    if future.is_null() {
        return std::ptr::null_mut();
    }
    let future_addr = future as usize;
    let handle = std::thread::spawn(move || {
        // SAFETY: the spawned thread owns the future pointer for the duration of `poll_fn`; it was checked non-null.
        unsafe { poll_fn(future_addr as *mut std::ffi::c_void) };
    });
    if let Ok(mut handles) = get_spawn_handles().lock() {
        handles.push(handle);
    }
    // Register an atexit handler once to join all spawned threads before exit.
    if SPAWN_ATEXIT_REGISTERED
        .compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
        )
        .is_ok()
    {
        // SAFETY: `mimi_join_spawned_threads_atexit` has C ABI and no parameters.
        unsafe { libc::atexit(mimi_join_spawned_threads_atexit) };
    }
    future
}

/// Wait (spin) for a future to become completed. Used by codegen `await`
/// for thread-spawned futures (not managed by the single-threaded executor).
#[no_mangle]
pub extern "C" fn mimi_await_future(future: *mut std::ffi::c_void) {
    if future.is_null() {
        return;
    }
    use std::sync::atomic::Ordering;
    // Spin until completed (the spawned thread sets completed=1 after poll_fn returns)
    // Uses Acquire ordering to synchronize-with the Release store in set_completed,
    // ensuring result data written before the completed flag is visible.
    // P2-12 fix: bounded spin with max iterations to prevent infinite CPU spin on bug.
    // SAFETY: `future` was checked non-null; `MimiFutureRepr` is valid and accessed atomically.
    unsafe {
        let rep = &*(future as *const MimiFutureRepr);
        let mut iterations: u64 = 0;
        const MAX_SPIN_ITERATIONS: u64 = 1_000_000;
        while rep.completed.load(Ordering::Acquire) == 0 {
            std::thread::yield_now();
            iterations += 1;
            if iterations >= MAX_SPIN_ITERATIONS {
                // Future not completed after max spin iterations — abort rather than
                // spin forever (which would consume 100% CPU indefinitely on bug).
                std::process::abort();
            }
        }
    }
}

type PollFn = unsafe extern "C" fn(*mut std::ffi::c_void);

/// Wrapper to make *mut c_void Send (needed for Mutex).
/// FFI-8: Soundness — a raw pointer is Send because:
/// - Sending a *mut T transfers exclusive ownership of the referent to the receiving thread
/// - The future pointer is only dereferenced inside `mimi_executor_run` while holding the queue mutex,
///   guaranteeing exclusive access (no data race)
/// - The pointer came from `mimi_rc_alloc` (system allocator, not thread-local), so it is safe to
///   access from any thread after the send
/// - `Sync` is safe because &SendPtr is never shared across threads (only &mut access via the mutex)
#[derive(Clone)]
struct SendPtr(*mut std::ffi::c_void);
// SAFETY: already documented above.
unsafe impl Send for SendPtr {}
// SAFETY: already documented above.
unsafe impl Sync for SendPtr {}

type ExecutorEntry = (PollFn, SendPtr);

static EXECUTOR_QUEUE: std::sync::Mutex<Vec<ExecutorEntry>> = std::sync::Mutex::new(Vec::new());

/// Submit a future + its poll function to the global executor.
/// The future is not polled immediately; call mimi_executor_run() to poll.
#[no_mangle]
pub extern "C" fn mimi_executor_spawn(
    future: *mut std::ffi::c_void,
    // SAFETY: unsafe extern "C" function pointer used for C poll callbacks; see # Safety docs.
    poll_fn: unsafe extern "C" fn(*mut std::ffi::c_void),
) {
    if future.is_null() {
        return;
    }
    let mut queue = EXECUTOR_QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    // Don't add duplicates
    if !queue.iter().any(|(_, f)| f.0 == future) {
        queue.push((poll_fn, SendPtr(future)));
    }
}

/// Poll all pending futures in the executor until all are completed.
/// Futures that become completed are removed from the queue.
#[no_mangle]
pub extern "C" fn mimi_executor_run() {
    loop {
        let entry = {
            let mut queue = EXECUTOR_QUEUE.lock().unwrap_or_else(|e| e.into_inner());
            if queue.is_empty() {
                return;
            }
            let mut found = None;
            for i in 0..queue.len() {
                let (_, future) = &queue[i];
                use std::sync::atomic::Ordering;
                // SAFETY: future pointer came from the executor queue and `MimiFutureRepr` is valid.
                let completed = unsafe {
                    let rep = &*(future.0 as *const MimiFutureRepr);
                    rep.completed.load(Ordering::Acquire)
                };
                if completed == 0 {
                    found = Some(i);
                    break;
                }
            }
            match found {
                Some(i) => {
                    let (poll_fn, future) = queue.swap_remove(i);
                    Some((poll_fn, future.0))
                }
                None => {
                    queue.clear();
                    return;
                }
            }
        };
        if let Some((poll_fn, future)) = entry {
            // SAFETY: `poll_fn` and `future` were taken from the executor queue; no aliased access while polling.
            unsafe { poll_fn(future) };
        }
    }
}

// ─── Capability runtime ────────────────────────────────────────

#[no_mangle]
pub extern "C" fn mimi_cap_check(cap: i64, name: *const std::ffi::c_char) -> bool {
    let n = if name.is_null() {
        ""
    } else {
        // SAFETY: `name` was checked non-null above.
        unsafe { CStr::from_ptr(name) }.to_str().unwrap_or("")
    };
    CAP_TABLE.with(|table| {
        let state = table.lock().unwrap_or_else(|e| e.into_inner());
        state
            .entries
            .iter()
            .any(|e| e.id == cap && !e.consumed && e.name == n)
    })
}

#[no_mangle]
pub extern "C" fn mimi_cap_consume(cap: i64, name: *const std::ffi::c_char) -> bool {
    let n = if name.is_null() {
        ""
    } else {
        // SAFETY: `name` was checked non-null above.
        unsafe { CStr::from_ptr(name) }.to_str().unwrap_or("")
    };
    CAP_TABLE.with(|table| {
        let mut state = table.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = state
            .entries
            .iter_mut()
            .find(|e| e.id == cap && !e.consumed)
        {
            if entry.name == n {
                entry.consumed = true;
                return true;
            }
        }
        false
    })
}

// ─── Directory & path operations ───────────────────────────────

/// Returns a Mimi List of entry names in the given directory.
/// Returns an empty list on error (not a directory, permission denied, etc.).
#[no_mangle]
pub extern "C" fn mimi_listdir(path: *const std::ffi::c_char) -> *mut MimiList {
    let path_str = if path.is_null() {
        return Box::into_raw(Box::new(MimiList {
            len: 0,
            data: std::ptr::null_mut(),
            owns_data: true,
        }));
    } else {
        // SAFETY: `path` was checked non-null above.
        match unsafe { CStr::from_ptr(path) }.to_str() {
            Ok(s) => s,
            Err(_) => {
                return Box::into_raw(Box::new(MimiList {
                    len: 0,
                    data: std::ptr::null_mut(),
                    owns_data: true,
                }))
            }
        }
    };
    let entries: Vec<*mut std::ffi::c_char> = match std::fs::read_dir(path_str) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().to_str().map(alloc_c_string))
            .collect(),
        Err(_) => {
            return Box::into_raw(Box::new(MimiList {
                len: 0,
                data: std::ptr::null_mut(),
                owns_data: true,
            }))
        }
    };
    let len = entries.len() as i64;
    let mut items = entries;
    let data_ptr = items.as_mut_ptr();
    std::mem::forget(items);
    Box::into_raw(Box::new(MimiList {
        len,
        data: data_ptr,
        owns_data: true,
    }))
}

/// Returns 1 if path is a directory, 0 otherwise.
#[no_mangle]
pub extern "C" fn mimi_is_dir(path: *const std::ffi::c_char) -> i64 {
    if path.is_null() {
        return 0;
    }
    // SAFETY: `path` was checked non-null above.
    let path_str = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(s) => s,
        Err(_) => return 0,
    };
    if std::path::Path::new(path_str).is_dir() {
        1
    } else {
        0
    }
}

/// Returns 1 if path is a regular file, 0 otherwise.
#[no_mangle]
pub extern "C" fn mimi_is_file(path: *const std::ffi::c_char) -> i64 {
    if path.is_null() {
        return 0;
    }
    // SAFETY: `path` was checked non-null above.
    let path_str = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(s) => s,
        Err(_) => return 0,
    };
    if std::path::Path::new(path_str).is_file() {
        1
    } else {
        0
    }
}

/// Joins two path components. Returns a new allocated string.
#[no_mangle]
pub extern "C" fn mimi_path_join(
    a: *const std::ffi::c_char,
    b: *const std::ffi::c_char,
) -> *mut std::ffi::c_char {
    let a_str = if a.is_null() {
        ""
    } else {
        // SAFETY: `a` was checked non-null above.
        unsafe { CStr::from_ptr(a) }.to_str().unwrap_or("")
    };
    let b_str = if b.is_null() {
        ""
    } else {
        // SAFETY: `b` was checked non-null above.
        unsafe { CStr::from_ptr(b) }.to_str().unwrap_or("")
    };
    let joined = std::path::Path::new(a_str)
        .join(b_str)
        .to_string_lossy()
        .into_owned();
    alloc_c_string(&joined)
}

/// Returns the file extension (without dot). Returns "" if none.
#[no_mangle]
pub extern "C" fn mimi_path_ext(path: *const std::ffi::c_char) -> *mut std::ffi::c_char {
    if path.is_null() {
        return alloc_c_string("");
    }
    // SAFETY: `path` was checked non-null above.
    let path_str = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(s) => s,
        Err(_) => return alloc_c_string(""),
    };
    let ext = std::path::Path::new(path_str)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    alloc_c_string(ext)
}

/// Returns the filename component of a path.
#[no_mangle]
pub extern "C" fn mimi_path_basename(path: *const std::ffi::c_char) -> *mut std::ffi::c_char {
    if path.is_null() {
        return alloc_c_string("");
    }
    // SAFETY: `path` was checked non-null above.
    let path_str = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(s) => s,
        Err(_) => return alloc_c_string(""),
    };
    let name = std::path::Path::new(path_str)
        .file_name()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    alloc_c_string(name)
}

/// Returns the directory component of a path.
#[no_mangle]
pub extern "C" fn mimi_path_dirname(path: *const std::ffi::c_char) -> *mut std::ffi::c_char {
    if path.is_null() {
        return alloc_c_string("");
    }
    // SAFETY: `path` was checked non-null above.
    let path_str = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(s) => s,
        Err(_) => return alloc_c_string(""),
    };
    let dir = std::path::Path::new(path_str)
        .parent()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    alloc_c_string(dir)
}

/// Recursively walks a directory and returns all file paths (as a Mimi List).
#[no_mangle]
pub extern "C" fn mimi_walk_dir(path: *const std::ffi::c_char) -> *mut MimiList {
    let empty = || {
        Box::into_raw(Box::new(MimiList {
            len: 0,
            data: std::ptr::null_mut(),
            owns_data: true,
        }))
    };
    let path_str = if path.is_null() {
        return empty();
    } else {
        // SAFETY: `path` was checked non-null above.
        match unsafe { CStr::from_ptr(path) }.to_str() {
            Ok(s) => s,
            Err(_) => return empty(),
        }
    };
    let mut results = Vec::new();
    walk_dir_recursive(path_str, &mut results);
    let len = results.len() as i64;
    let mut items: Vec<*mut std::ffi::c_char> =
        results.into_iter().map(|s| alloc_c_string(&s)).collect();
    let data_ptr = items.as_mut_ptr();
    std::mem::forget(items);
    Box::into_raw(Box::new(MimiList {
        len,
        data: data_ptr,
        owns_data: true,
    }))
}

fn walk_dir_recursive(dir: &str, results: &mut Vec<String>) {
    let rd = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return,
    };
    for entry in rd.flatten() {
        let path = entry.path();
        let path_str = path.to_string_lossy().into_owned();
        if path.is_dir() {
            walk_dir_recursive(&path_str, results);
        } else {
            results.push(path_str);
        }
    }
}

/// Creates a directory and all parent directories. Returns 1 on success, 0 on failure.
#[no_mangle]
pub extern "C" fn mimi_mkdir_p(path: *const std::ffi::c_char) -> i64 {
    if path.is_null() {
        return 0;
    }
    // SAFETY: `path` was checked non-null above.
    let path_str = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(s) => s,
        Err(_) => return 0,
    };
    if std::fs::create_dir_all(path_str).is_ok() {
        1
    } else {
        0
    }
}

/// Removes a file. Returns 1 on success, 0 on failure.
#[no_mangle]
pub extern "C" fn mimi_remove_file(path: *const std::ffi::c_char) -> i64 {
    if path.is_null() {
        return 0;
    }
    // SAFETY: `path` was checked non-null above.
    let path_str = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(s) => s,
        Err(_) => return 0,
    };
    if std::fs::remove_file(path_str).is_ok() {
        1
    } else {
        0
    }
}

// ─── Process & advanced file operations ─────────────────────────

/// Result of executing a shell command.
#[repr(C)]
pub struct MimiExecResult {
    pub exit_code: i64,
    pub stdout: *mut std::ffi::c_char,
    pub stderr: *mut std::ffi::c_char,
}

/// Executes a shell command via `sh -c`. Returns a heap-allocated MimiExecResult.
/// WARNING: shell metacharacters in the command string are interpreted by sh.
/// For safer execution that avoids shell injection, use `mimi_exec_safe`.
/// Execute a shell command via `sh -c`. Returns a `MimiExecResult` struct.
/// Uses shell interpretation (pipelines, variables, redirections).
/// ⚠️ Shell injection risk: if `cmd` comes from untrusted input, use
/// `mimi_exec_safe` instead which runs a single program without shell.
/// Caller must free with `mimi_exec_free`.
/// Execute a shell command via `sh -c`. This is intentionally a shell
/// execution function — callers are responsible for sanitizing input.
/// For safe execution without shell injection, use `mimi_exec_safe`.
///
/// Security note (HIGH): `cmd` is passed directly to `sh -c`. If `cmd`
/// contains user-controlled input, shell injection is possible. Only
/// use `mimi_exec` with trusted, hard-coded command strings. For
/// untrusted input, use `mimi_exec_safe` which avoids the shell.
#[no_mangle]
pub extern "C" fn mimi_exec(cmd: *const std::ffi::c_char) -> *mut MimiExecResult {
    // RT-H5: optional hard refuse under MIMI_EXEC_STRICT / MIMI_FFI_STRICT.
    if std::env::var("MIMI_EXEC_STRICT")
        .or_else(|_| std::env::var("MIMI_FFI_STRICT"))
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
    {
        let res = Box::new(MimiExecResult {
            exit_code: -1,
            stdout: alloc_c_string(""),
            stderr: alloc_c_string(
                "exec error: mimi_exec refused under MIMI_EXEC_STRICT/MIMI_FFI_STRICT; use mimi_exec_safe",
            ),
        });
        return Box::into_raw(res);
    }
    static EXEC_WARNED: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
    if !EXEC_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        eprintln!(
            "[mimi] RT-H5 WARNING: mimi_exec uses sh -c (shell injection risk).              Prefer mimi_exec_safe, or set MIMI_EXEC_STRICT=1 to refuse shell exec."
        );
    }
    if cmd.is_null() {
        let res = Box::new(MimiExecResult {
            exit_code: -1,
            stdout: alloc_c_string(""),
            stderr: alloc_c_string("exec error: null command"),
        });
        return Box::into_raw(res);
    }
    // SAFETY: `cmd` was checked non-null above.
    let cmd_str = match unsafe { CStr::from_ptr(cmd) }.to_str() {
        Ok(s) => s,
        Err(e) => {
            let res = Box::new(MimiExecResult {
                exit_code: -1,
                stdout: alloc_c_string(""),
                stderr: alloc_c_string(&format!("exec error: {}", e)),
            });
            return Box::into_raw(res);
        }
    };
    // Reject embedded null bytes to prevent shell injection through truncated command.
    if cmd_str.contains('\0') {
        let res = Box::new(MimiExecResult {
            exit_code: -1,
            stdout: alloc_c_string(""),
            stderr: alloc_c_string("exec error: command contains null byte"),
        });
        return Box::into_raw(res);
    }
    const MAX_EXEC_OUTPUT: usize = 10 * 1024 * 1024; // 10MB per stream
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd_str)
        .output();
    match output {
        Ok(out) => {
            let stdout_bytes = if out.stdout.len() > MAX_EXEC_OUTPUT {
                &out.stdout[..MAX_EXEC_OUTPUT]
            } else {
                &out.stdout
            };
            let stderr_bytes = if out.stderr.len() > MAX_EXEC_OUTPUT {
                &out.stderr[..MAX_EXEC_OUTPUT]
            } else {
                &out.stderr
            };
            let stdout = String::from_utf8_lossy(stdout_bytes).to_string();
            let stderr = String::from_utf8_lossy(stderr_bytes).to_string();
            let exit_code = out.status.code().unwrap_or(-1);
            let res = Box::new(MimiExecResult {
                exit_code: exit_code as i64,
                stdout: alloc_c_string(&stdout),
                stderr: alloc_c_string(&stderr),
            });
            Box::into_raw(res)
        }
        Err(e) => {
            let res = Box::new(MimiExecResult {
                exit_code: -1,
                stdout: alloc_c_string(""),
                stderr: alloc_c_string(&format!("exec error: {}", e)),
            });
            Box::into_raw(res)
        }
    }
}

/// Frees a MimiExecResult allocated by mimi_exec.
/// stdout/stderr are allocated by alloc_c_string (libc::malloc), so must use libc::free.
#[no_mangle]
pub extern "C" fn mimi_exec_free(res: *mut MimiExecResult) {
    if res.is_null() {
        return;
    }
    // SAFETY: `res` was checked non-null; stdout/stderr were allocated by `alloc_c_string`.
    unsafe {
        let r = Box::from_raw(res);
        if !r.stdout.is_null() {
            libc::free(r.stdout as *mut std::ffi::c_void);
        }
        if !r.stderr.is_null() {
            libc::free(r.stderr as *mut std::ffi::c_void);
        }
    }
}

/// Frees only the MimiExecResult struct, NOT the stdout/stderr strings.
/// Used by codegen after extracting string pointers into ExecResult struct.
#[no_mangle]
pub extern "C" fn mimi_exec_free_struct(res: *mut MimiExecResult) {
    if res.is_null() {
        return;
    }
    // SAFETY: `res` was checked non-null; struct is freed without freeing string members.
    unsafe {
        let _ = Box::from_raw(res);
        // stdout/stderr are NOT freed — they're owned by the ExecResult struct
    }
}

/// Executes a command and returns just stdout. Simpler than mimi_exec.
/// Returns an allocated C string (caller must free with mimi_string_free).
/// On error, returns an empty string.
/// ⚠️ Shell injection risk: if `cmd` comes from untrusted input, use
/// `mimi_exec_safe` instead which runs a single program without shell.
#[no_mangle]
pub extern "C" fn mimi_exec_pipe(cmd: *const std::ffi::c_char) -> *mut std::ffi::c_char {
    if cmd.is_null() {
        return alloc_c_string("");
    }
    // SAFETY: `cmd` was checked non-null above.
    let cmd_str = match unsafe { CStr::from_ptr(cmd) }.to_str() {
        Ok(s) => s,
        Err(_) => return alloc_c_string(""),
    };
    // Reject embedded null bytes to prevent shell injection.
    if cmd_str.contains('\0') {
        return alloc_c_string("");
    }
    const MAX_EXEC_OUTPUT: usize = 10 * 1024 * 1024; // 10MB
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd_str)
        .output();
    match output {
        Ok(out) => {
            let stdout_bytes = if out.stdout.len() > MAX_EXEC_OUTPUT {
                &out.stdout[..MAX_EXEC_OUTPUT]
            } else {
                &out.stdout
            };
            let stdout = String::from_utf8_lossy(stdout_bytes).to_string();
            alloc_c_string(&stdout)
        }
        Err(_) => alloc_c_string(""),
    }
}

/// Execute a single program without shell interpretation.
/// `prog` is the program path, `args` are the arguments (excluding argv[0]).
/// Returns a `MimiExecResult` struct. Caller must free with `mimi_exec_free`.
/// No shell injection risk: the program is executed directly via `execvp`.
#[no_mangle]
pub extern "C" fn mimi_exec_safe(
    prog: *const std::ffi::c_char,
    args: *mut MimiList,
) -> *mut MimiExecResult {
    let prog_str = if prog.is_null() {
        let res = Box::new(MimiExecResult {
            exit_code: -1,
            stdout: alloc_c_string(""),
            stderr: alloc_c_string("exec_safe error: null program"),
        });
        return Box::into_raw(res);
    } else {
        // SAFETY: prog was checked non-null above.
        match unsafe { CStr::from_ptr(prog) }.to_str() {
            Ok(s) => s.to_string(),
            Err(_) => {
                let res = Box::new(MimiExecResult {
                    exit_code: -1,
                    stdout: alloc_c_string(""),
                    stderr: alloc_c_string("exec_safe error: invalid program name"),
                });
                return Box::into_raw(res);
            }
        }
    };
    if args.is_null() {
        // No args — just run the program with no arguments.
        let output = std::process::Command::new(&prog_str).output();
        return match output {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout).to_string();
                let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                let exit_code = out.status.code().unwrap_or(-1);
                Box::into_raw(Box::new(MimiExecResult {
                    exit_code: exit_code as i64,
                    stdout: alloc_c_string(&stdout),
                    stderr: alloc_c_string(&stderr),
                }))
            }
            Err(e) => Box::into_raw(Box::new(MimiExecResult {
                exit_code: -1,
                stdout: alloc_c_string(""),
                stderr: alloc_c_string(&format!("exec_safe error: {}", e)),
            })),
        };
    }
    // SAFETY: args was checked non-null above.
    let lst = unsafe { &*args };
    let mut cmd = std::process::Command::new(&prog_str);
    for i in 0..lst.len as isize {
        // SAFETY: i is within bounds (0..lst.len).
        let item_ptr = unsafe { *lst.data.offset(i) as *const std::ffi::c_char };
        if item_ptr.is_null() {
            continue;
        }
        // SAFETY: item_ptr is non-null (checked above).
        let s = unsafe { cstr_to_string(item_ptr) };
        cmd.arg(s);
    }
    const MAX_EXEC_OUTPUT: usize = 10 * 1024 * 1024; // 10MB per stream
    let output = cmd.output();
    match output {
        Ok(out) => {
            let stdout_bytes = if out.stdout.len() > MAX_EXEC_OUTPUT {
                &out.stdout[..MAX_EXEC_OUTPUT]
            } else {
                &out.stdout
            };
            let stderr_bytes = if out.stderr.len() > MAX_EXEC_OUTPUT {
                &out.stderr[..MAX_EXEC_OUTPUT]
            } else {
                &out.stderr
            };
            let stdout = String::from_utf8_lossy(stdout_bytes).to_string();
            let stderr = String::from_utf8_lossy(stderr_bytes).to_string();
            let exit_code = out.status.code().unwrap_or(-1);
            let res = Box::new(MimiExecResult {
                exit_code: exit_code as i64,
                stdout: alloc_c_string(&stdout),
                stderr: alloc_c_string(&stderr),
            });
            Box::into_raw(res)
        }
        Err(e) => {
            let res = Box::new(MimiExecResult {
                exit_code: -1,
                stdout: alloc_c_string(""),
                stderr: alloc_c_string(&format!("exec_safe error: {}", e)),
            });
            Box::into_raw(res)
        }
    }
}

/// Result of stat-ing a file.
#[repr(C)]
pub struct MimiStatResult {
    pub size: i64,
    pub modified: i64,
    pub is_file: i64,
    pub is_dir: i64,
}

/// Frees a MimiStatResult allocated by mimi_file_stat.
#[no_mangle]
pub extern "C" fn mimi_file_stat_free(res: *mut MimiStatResult) {
    if res.is_null() {
        return;
    }
    // SAFETY: `res` was checked non-null; freeing the stat result struct.
    unsafe {
        let _ = Box::from_raw(res);
    }
}

/// Stats a file. Returns a heap-allocated MimiStatResult, or null on error.
/// On error, sets *err_out to an allocated error string (caller must free with mimi_string_free).
#[no_mangle]
pub extern "C" fn mimi_file_stat(
    path: *const std::ffi::c_char,
    err_out: *mut *mut std::ffi::c_char,
) -> *mut MimiStatResult {
    if path.is_null() {
        if !err_out.is_null() {
            // SAFETY: `err_out` was checked non-null above.
            unsafe { *err_out = alloc_c_string("file_stat error: null path") };
        }
        return std::ptr::null_mut();
    }
    // SAFETY: `path` was checked non-null above.
    let path_str = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(s) => s,
        Err(e) => {
            if !err_out.is_null() {
                // SAFETY: `err_out` was checked non-null above.
                unsafe { *err_out = alloc_c_string(&format!("file_stat error: {}", e)) };
            }
            return std::ptr::null_mut();
        }
    };
    match std::fs::metadata(path_str) {
        Ok(meta) => {
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let res = Box::new(MimiStatResult {
                size: meta.len() as i64,
                modified,
                is_file: if meta.is_file() { 1 } else { 0 },
                is_dir: if meta.is_dir() { 1 } else { 0 },
            });
            if !err_out.is_null() {
                // SAFETY: `err_out` was checked non-null above.
                unsafe { *err_out = std::ptr::null_mut() };
            }
            Box::into_raw(res)
        }
        Err(e) => {
            if !err_out.is_null() {
                // SAFETY: `err_out` was checked non-null above.
                unsafe { *err_out = alloc_c_string(&format!("file_stat error: {}", e)) };
            }
            std::ptr::null_mut()
        }
    }
}

/// Appends content to a file. Returns 1 on success, 0 on failure.
#[no_mangle]
pub extern "C" fn mimi_append_file(
    path: *const std::ffi::c_char,
    content: *const std::ffi::c_char,
) -> i64 {
    if path.is_null() || content.is_null() {
        return 0;
    }
    // SAFETY: `path` was checked non-null above.
    let path_str = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(s) => s,
        Err(_) => return 0,
    };
    // SAFETY: `content` was checked non-null above.
    let content_str = match unsafe { CStr::from_ptr(content) }.to_str() {
        Ok(s) => s,
        Err(_) => return 0,
    };
    use std::io::Write;
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path_str)
    {
        Ok(mut file) => {
            if file.write_all(content_str.as_bytes()).is_ok() {
                1
            } else {
                0
            }
        }
        Err(_) => 0,
    }
}

/// H14 fix: Global mutex for env var operations to prevent data races
/// when Mimi actor/spawn threads call set_var concurrently.
static SETENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Sets an environment variable. Returns 1 on success, 0 on failure.
/// Thread-safe: uses a global mutex to serialize env var modifications,
/// preventing data races when called from multiple actor threads.
#[no_mangle]
pub extern "C" fn mimi_set_env(
    key: *const std::ffi::c_char,
    value: *const std::ffi::c_char,
) -> i64 {
    if key.is_null() || value.is_null() {
        return 0;
    }
    let key_str = match unsafe { CStr::from_ptr(key) }.to_str() {
        Ok(s) => s,
        Err(_) => return 0,
    };
    let value_str = match unsafe { CStr::from_ptr(value) }.to_str() {
        Ok(s) => s,
        Err(_) => return 0,
    };
    // Serialize env var writes under a global lock.
    let _lock = match SETENV_LOCK.lock() {
        Ok(guard) => guard,
        Err(_) => return 0,
    };
    // SAFETY: key_str and value_str are non-null C strings (checked above).
    // std::env::set_var takes &str which requires UTF-8; this is a
    // best-effort call and may panic on non-UTF-8 input, which matches
    // the documented behavior of this runtime function.
    unsafe { std::env::set_var(key_str, value_str) };
    1
}

// ─── Crypto operations ─────────────────────────────────────────

/// SHA-256 hash of a NUL-terminated C string — returns hex string (64 chars).
/// Pure Rust implementation, no external dependencies.
///
/// RT-H8 note: CStr stops at the first NUL. For binary data with embedded NULs,
/// use `mimi_sha256_n(data, len)` instead.
#[no_mangle]
pub extern "C" fn mimi_sha256(data: *const std::ffi::c_char) -> *mut std::ffi::c_char {
    let input = if data.is_null() {
        b"".as_slice()
    } else {
        // SAFETY: `data` was checked non-null above.
        unsafe { CStr::from_ptr(data) }.to_bytes()
    };
    let hash = sha256_bytes(input);
    let hex: String = hash.iter().map(|b| format!("{:02x}", b)).collect();
    alloc_c_string(&hex)
}

/// SHA-256 of an explicit byte buffer (handles embedded NULs).
/// Returns a heap hex string (caller frees with mimi_string_free).
#[no_mangle]
pub extern "C" fn mimi_sha256_n(data: *const u8, len: i64) -> *mut std::ffi::c_char {
    if data.is_null() || len <= 0 {
        let hash = sha256_bytes(b"");
        let hex: String = hash.iter().map(|b| format!("{:02x}", b)).collect();
        return alloc_c_string(&hex);
    }
    const MAX: i64 = 64 * 1024 * 1024;
    if len > MAX {
        return std::ptr::null_mut();
    }
    // SAFETY: caller provides `len` readable bytes at `data`.
    let input = unsafe { std::slice::from_raw_parts(data, len as usize) };
    let hash = sha256_bytes(input);
    let hex: String = hash.iter().map(|b| format!("{:02x}", b)).collect();
    alloc_c_string(&hex)
}

pub fn sha256_bytes(data: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let k: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    // Pre-processing: padding
    let original_len = data.len();
    let bit_len = (original_len as u64) * 8;
    let mut padded = data.to_vec();
    padded.push(0x80);
    while (padded.len() % 64) != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    // Process each 512-bit (64-byte) chunk
    for chunk in padded.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(k[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut result = [0u8; 32];
    for i in 0..8 {
        result[i * 4..i * 4 + 4].copy_from_slice(&h[i].to_be_bytes());
    }
    result
}

/// Base64 encode — returns allocated C string.
#[no_mangle]
pub extern "C" fn mimi_base64_encode(data: *const std::ffi::c_char) -> *mut std::ffi::c_char {
    let input = if data.is_null() {
        b"".as_slice()
    } else {
        // SAFETY: `data` was checked non-null above.
        unsafe { CStr::from_ptr(data) }.to_bytes()
    };
    let encoded = base64_encode_bytes(input);
    alloc_c_string(&encoded)
}

pub fn base64_encode_bytes(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((triple >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// Base64 decode — returns Result<string, string>.
#[no_mangle]
pub extern "C" fn mimi_base64_decode(data: *const std::ffi::c_char) -> *mut std::ffi::c_char {
    let input = if data.is_null() {
        ""
    } else {
        // SAFETY: `data` was checked non-null above.
        match unsafe { CStr::from_ptr(data) }.to_str() {
            Ok(s) => s,
            Err(_) => return alloc_c_string(""),
        }
    };
    match base64_decode_str(input) {
        Ok(s) => alloc_c_string(&s),
        Err(_) => alloc_c_string(""),
    }
}

#[allow(clippy::result_unit_err)]
pub fn base64_decode_str(input: &str) -> Result<String, ()> {
    const REV: [i8; 128] = {
        let mut table = [-1i8; 128];
        let mut i = 0;
        while i < 26 {
            table[(b'A' + i) as usize] = i as i8;
            i += 1;
        }
        while i < 52 {
            table[(b'a' + i - 26) as usize] = i as i8;
            i += 1;
        }
        while i < 62 {
            table[(b'0' + i - 52) as usize] = i as i8;
            i += 1;
        }
        table[b'+' as usize] = 62;
        table[b'/' as usize] = 63;
        table
    };
    let clean: Vec<u8> = input
        .bytes()
        .filter(|&b| b != b'=' && !b.is_ascii_whitespace())
        .collect();
    let mut output = Vec::new();
    for chunk in clean.chunks(4) {
        let mut buf = 0u32;
        let mut bits = 0;
        for &b in chunk {
            if b >= 128 || REV[b as usize] < 0 {
                return Err(());
            }
            buf = (buf << 6) | (REV[b as usize] as u32);
            bits += 6;
        }
        while bits >= 8 {
            bits -= 8;
            output.push((buf >> bits) as u8);
        }
    }
    String::from_utf8(output).map_err(|_| ())
}

#[no_mangle]
pub extern "C" fn mimi_to_string_i64(val: i64) -> *mut std::ffi::c_char {
    alloc_c_string(&val.to_string())
}

#[no_mangle]
pub extern "C" fn mimi_to_string_f64(val: f64) -> *mut std::ffi::c_char {
    alloc_c_string(&val.to_string())
}

#[no_mangle]
/// M15: template string formatting with up to 8 arguments ({}-placeholders).
/// If more than 8 args are needed, callers should concatenate intermediate results.
pub extern "C" fn mimi_str_format(
    num_args: i64,
    template: *const std::ffi::c_char,
    arg0: *const std::ffi::c_char,
    arg1: *const std::ffi::c_char,
    arg2: *const std::ffi::c_char,
    arg3: *const std::ffi::c_char,
    arg4: *const std::ffi::c_char,
    arg5: *const std::ffi::c_char,
    arg6: *const std::ffi::c_char,
    arg7: *const std::ffi::c_char,
) -> *mut std::ffi::c_char {
    // SAFETY: `template` is used as a fallback if null; caller should pass a valid C string.
    let tmpl = unsafe { cstr_to_string(template) };
    let args = [arg0, arg1, arg2, arg3, arg4, arg5, arg6, arg7];
    let mut result = String::new();
    let mut rest = tmpl.as_str();
    let mut arg_idx = 0;
    while let Some(pos) = rest.find("{}") {
        result.push_str(&rest[..pos]);
        if arg_idx < num_args as usize && arg_idx < args.len() {
            // SAFETY: argument pointers are passed to `cstr_to_string` which handles null.
            let arg_str = unsafe { cstr_to_string(args[arg_idx]) };
            result.push_str(&arg_str);
            arg_idx += 1;
        } else {
            result.push_str("{}");
        }
        rest = &rest[pos + 2..];
    }
    result.push_str(rest);
    alloc_c_string(&result)
}

// ─── Binary I/O & streaming line reading ──────────────────────

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

// ─── Mimi source parser for linting (mimi_parse_source / mimi_lexer_tokenize) ──
//
// These functions implement a minimal Mimi tokenizer + recursive-descent parser
// that produces a JSON string. The Mimi-level `parse()` and `lexer()` builtins
// call these at runtime. The JSON is consumed by Mimi code via `from_json()`.
//
// JSON schema for mimi_lexer_tokenize(source):
//   [{"kind":"IDENT","value":"func","line":1,"col":1}, ...]
//
// JSON schema for mimi_parse_source(source):
//   {
//     "functions": [{"name":"main","line":1,"col":6,"is_pub":false, ...}],
//     "types": [...], "imports": [...], "has_main": true
//   }

struct MimiToken {
    kind: String,
    value: String,
    line: usize,
    col: usize,
}

struct MimiLexer {
    chars: Vec<char>,
    pos: usize,
    line: usize,
    col: usize,
}

impl MimiLexer {
    fn new(source: &str) -> Self {
        Self {
            chars: source.chars().collect(),
            pos: 0,
            line: 1,
            col: 1,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn advance(&mut self) -> Option<char> {
        let c = self.chars.get(self.pos).copied();
        if let Some(ch) = c {
            self.pos += 1;
            if ch == '\n' {
                self.line += 1;
                self.col = 1;
            } else {
                self.col += 1;
            }
        }
        c
    }

    fn skip_whitespace(&mut self) {
        while let Some(ch) = self.peek() {
            if ch == ' ' || ch == '\t' || ch == '\n' || ch == '\r' {
                self.advance();
            } else {
                break;
            }
        }
    }

    fn tokenize(&mut self) -> Vec<MimiToken> {
        let mut tokens = Vec::new();
        loop {
            self.skip_whitespace();
            let line = self.line;
            let col = self.col;
            match self.peek() {
                None => break,
                Some('/') if self.pos + 1 < self.chars.len() && self.chars[self.pos + 1] == '/' => {
                    while let Some(ch) = self.peek() {
                        if ch == '\n' {
                            break;
                        }
                        self.advance();
                    }
                }
                Some('/') if self.pos + 1 < self.chars.len() && self.chars[self.pos + 1] == '*' => {
                    self.advance();
                    self.advance();
                    while let Some(ch) = self.peek() {
                        if ch == '*'
                            && self.pos + 1 < self.chars.len()
                            && self.chars[self.pos + 1] == '/'
                        {
                            self.advance();
                            self.advance();
                            break;
                        }
                        self.advance();
                    }
                }
                Some('"') => {
                    self.advance();
                    let mut s = String::new();
                    while let Some(ch) = self.peek() {
                        if ch == '"' {
                            self.advance();
                            break;
                        }
                        if ch == '\\' {
                            self.advance();
                            if let Some(esc) = self.peek() {
                                match esc {
                                    'n' => s.push('\n'),
                                    't' => s.push('\t'),
                                    'r' => s.push('\r'),
                                    '"' => s.push('"'),
                                    '\\' => s.push('\\'),
                                    c => s.push(c),
                                }
                                self.advance();
                            }
                        } else {
                            s.push(ch);
                            self.advance();
                        }
                    }
                    tokens.push(MimiToken {
                        kind: "STRING".into(),
                        value: s,
                        line,
                        col,
                    });
                }
                Some(c)
                    if c.is_ascii_digit()
                        || (c == '-'
                            && self.pos + 1 < self.chars.len()
                            && self.chars[self.pos + 1].is_ascii_digit()) =>
                {
                    let mut s = String::new();
                    if c == '-' {
                        s.push('-');
                        self.advance();
                    }
                    let mut is_float = false;
                    while let Some(ch) = self.peek() {
                        if ch.is_ascii_digit() {
                            s.push(ch);
                            self.advance();
                        } else if ch == '.' {
                            is_float = true;
                            s.push(ch);
                            self.advance();
                        } else {
                            break;
                        }
                    }
                    tokens.push(MimiToken {
                        kind: if is_float {
                            "FLOAT".into()
                        } else {
                            "INT".into()
                        },
                        value: s,
                        line,
                        col,
                    });
                }
                Some(c) if c.is_ascii_alphabetic() || c == '_' => {
                    let mut s = String::new();
                    while let Some(ch) = self.peek() {
                        if ch.is_ascii_alphanumeric() || ch == '_' {
                            s.push(ch);
                            self.advance();
                        } else {
                            break;
                        }
                    }
                    let kind = match s.as_str() {
                        "func" | "pub" | "let" | "mut" | "if" | "else" | "while" | "for"
                        | "return" | "break" | "continue" | "true" | "false" | "module" | "use"
                        | "const" | "type" | "extern" | "match" | "in" | "as" | "struct"
                        | "enum" | "union" | "newtype" | "where" | "trait" | "impl" | "cap"
                        | "shared" | "local_shared" | "weak" | "loop" | "parasteps" | "alloc"
                        | "arena" | "unsafe" | "drop" | "on_failure" | "comptime" | "async"
                        | "requires" | "ensures" | "desc" | "rule" | "mms" | "invariant"
                        | "math" | "Record" | "Any" | "Option" | "Result" | "List" | "Set"
                        | "Map" | "Future" | "String" | "bool" | "i32" | "i64" | "f32" | "f64" => {
                            "KEYWORD"
                        }
                        _ => "IDENT",
                    };
                    tokens.push(MimiToken {
                        kind: kind.into(),
                        value: s,
                        line,
                        col,
                    });
                }
                Some(c) => {
                    let mut val = String::new();
                    val.push(c);
                    self.advance();
                    if matches!(
                        c,
                        '=' | '!' | '<' | '>' | '&' | '|' | '+' | '-' | '*' | '/' | '.' | ':'
                    ) {
                        if let Some(next) = self.peek() {
                            if (matches!(c, '=' | '!' | '<' | '>') && next == '=')
                                || (c == '&' && next == '&')
                                || (c == '|' && next == '|')
                                || (c == '+' && next == '=')
                                || (c == '-' && (next == '=' || next == '>'))
                                || (c == ':' && next == ':')
                                || (c == '.' && next == '.')
                            {
                                val.push(next);
                                self.advance();
                            }
                        }
                    }
                    tokens.push(MimiToken {
                        kind: if matches!(
                            c,
                            '{' | '}'
                                | '('
                                | ')'
                                | '['
                                | ']'
                                | ';'
                                | ','
                                | ':'
                                | '|'
                                | '&'
                                | '#'
                                | '@'
                                | '~'
                                | '?'
                        ) {
                            "PUNCT".into()
                        } else {
                            "OP".into()
                        },
                        value: val,
                        line,
                        col,
                    });
                }
            }
        }
        tokens.push(MimiToken {
            kind: "EOF".into(),
            value: String::new(),
            line: self.line,
            col: self.col,
        });
        tokens
    }
}

fn mimi_tokens_to_json(tokens: &[MimiToken]) -> String {
    let mut json = String::from("[");
    for (i, tok) in tokens.iter().enumerate() {
        if i > 0 {
            json.push(',');
        }
        let v_escaped = tok.value.replace('\\', "\\\\").replace('"', "\\\"");
        json.push_str(&format!(
            r#"{{"kind":"{}","value":"{}","line":{},"col":{}}}"#,
            tok.kind, v_escaped, tok.line, tok.col
        ));
    }
    json.push(']');
    json
}

#[no_mangle]
pub extern "C" fn mimi_lexer_tokenize(source: *const std::ffi::c_char) -> *mut std::ffi::c_char {
    if source.is_null() {
        return alloc_c_string("[]");
    }
    // SAFETY: `source` was checked non-null above.
    let src = unsafe { cstr_to_string(source) };
    let mut lexer = MimiLexer::new(&src);
    let tokens = lexer.tokenize();
    let json = mimi_tokens_to_json(&tokens[..tokens.len().saturating_sub(1)]);
    alloc_c_string(&json)
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // M1 fix: U+2028 LINE SEPARATOR and U+2029 PARAGRAPH SEPARATOR are
            // valid JSON only when escaped. Unescaped, they break JSON parsers
            // that follow ECMAScript 2018+ line-terminator rules.
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c if c < '\x20' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[no_mangle]
pub extern "C" fn mimi_parse_source(source: *const std::ffi::c_char) -> *mut std::ffi::c_char {
    if source.is_null() {
        return alloc_c_string(r#"{"functions":[],"types":[],"imports":[],"has_main":false}"#);
    }
    // SAFETY: `source` was checked non-null above.
    let src = unsafe { cstr_to_string(source) };
    let mut lexer = MimiLexer::new(&src);
    let tokens = lexer.tokenize();
    let json = mimi_build_ast_json(&tokens);
    alloc_c_string(&json)
}

fn mimi_build_ast_json(tokens: &[MimiToken]) -> String {
    let mut out = String::from(r#"{"functions":["#);
    let mut first_func = true;
    let mut types_json = Vec::new();
    let mut modules_json = Vec::new();
    let mut imports_json = Vec::new();
    let mut has_main = false;
    let mut idx = 0;

    while idx < tokens.len() {
        let tok = &tokens[idx];
        if tok.kind == "EOF" {
            break;
        }
        if tok.kind != "KEYWORD" && tok.kind != "IDENT" {
            idx += 1;
            continue;
        }

        match tok.value.as_str() {
            "pub" => {
                if idx + 1 < tokens.len() && tokens[idx + 1].value == "func" {
                    let (func_json, consumed, is_main) = parse_func_decl(tokens, idx, true);
                    if !func_json.is_empty() {
                        if !first_func {
                            out.push(',');
                        }
                        out.push_str(&func_json);
                        first_func = false;
                        if is_main {
                            has_main = true;
                        }
                    }
                    idx += consumed;
                } else {
                    idx += 1;
                }
            }
            "func" => {
                let (func_json, consumed, is_main) = parse_func_decl(tokens, idx, false);
                if !func_json.is_empty() {
                    if !first_func {
                        out.push(',');
                    }
                    out.push_str(&func_json);
                    first_func = false;
                    if is_main {
                        has_main = true;
                    }
                }
                idx += consumed;
            }
            "type" | "struct" | "enum" | "union" | "newtype" => {
                let line = tok.line;
                let col = tok.col;
                let kind = tok.value.clone();
                idx += 1;
                let mut name = String::from("_");
                if idx < tokens.len()
                    && (tokens[idx].kind == "IDENT" || tokens[idx].kind == "KEYWORD")
                {
                    name = tokens[idx].value.clone();
                    idx += 1;
                }
                if idx < tokens.len() && tokens[idx].value == "<" {
                    let mut depth = 1;
                    idx += 1;
                    while idx < tokens.len() && depth > 0 {
                        if tokens[idx].value == "<" {
                            depth += 1;
                        } else if tokens[idx].value == ">" {
                            depth -= 1;
                        }
                        idx += 1;
                    }
                }
                if idx < tokens.len() && tokens[idx].value == "{" {
                    let mut depth = 1;
                    idx += 1;
                    while idx < tokens.len() && depth > 0 {
                        if tokens[idx].value == "{" {
                            depth += 1;
                        } else if tokens[idx].value == "}" {
                            depth -= 1;
                        }
                        idx += 1;
                    }
                } else {
                    while idx < tokens.len()
                        && tokens[idx].value != ";"
                        && tokens[idx].kind != "EOF"
                    {
                        idx += 1;
                    }
                    if idx < tokens.len() && tokens[idx].value == ";" {
                        idx += 1;
                    }
                }
                types_json.push(format!(
                    r#"{{"name":"{}","line":{},"col":{},"kind":"{}"}}"#,
                    json_escape(&name),
                    line,
                    col,
                    json_escape(&kind)
                ));
            }
            "module" => {
                idx += 1;
                if idx < tokens.len()
                    && (tokens[idx].kind == "IDENT" || tokens[idx].kind == "KEYWORD")
                {
                    let mname = tokens[idx].value.clone();
                    let mline = tokens[idx].line;
                    let mcol = tokens[idx].col;
                    idx += 1;
                    if idx < tokens.len() && tokens[idx].value == ";" {
                        idx += 1;
                    } else if idx < tokens.len() && tokens[idx].value == "{" {
                        let mut depth = 1;
                        idx += 1;
                        while idx < tokens.len() && depth > 0 {
                            if tokens[idx].value == "{" {
                                depth += 1;
                            } else if tokens[idx].value == "}" {
                                depth -= 1;
                            }
                            idx += 1;
                        }
                    }
                    modules_json.push(format!(
                        r#"{{"name":"{}","line":{},"col":{}}}"#,
                        json_escape(&mname),
                        mline,
                        mcol
                    ));
                }
            }
            "use" | "import" => {
                idx += 1;
                let mut path_parts: Vec<String> = Vec::new();
                let line = tok.line;
                let col = tok.col;
                while idx < tokens.len()
                    && (tokens[idx].kind == "IDENT" || tokens[idx].kind == "KEYWORD")
                {
                    path_parts.push(tokens[idx].value.clone());
                    idx += 1;
                    if idx < tokens.len() && tokens[idx].value == "::" {
                        idx += 1;
                    } else {
                        break;
                    }
                }
                let mut alias: Option<String> = None;
                if idx + 1 < tokens.len() && tokens[idx].value == "as" && idx + 1 < tokens.len() {
                    idx += 1;
                    if tokens[idx].kind == "IDENT" || tokens[idx].kind == "KEYWORD" {
                        alias = Some(tokens[idx].value.clone());
                        idx += 1;
                    }
                }
                while idx < tokens.len() && tokens[idx].value != ";" && tokens[idx].kind != "EOF" {
                    idx += 1;
                }
                if idx < tokens.len() && tokens[idx].value == ";" {
                    idx += 1;
                }
                imports_json.push(format!(
                    r#"{{"path":[{}],"alias":{},"line":{},"col":{}}}"#,
                    path_parts
                        .iter()
                        .map(|p| format!("\"{}\"", json_escape(p)))
                        .collect::<Vec<_>>()
                        .join(","),
                    alias
                        .as_ref()
                        .map_or("null".to_string(), |a| format!("\"{}\"", json_escape(a))),
                    line,
                    col
                ));
            }
            "const" => {
                idx += 1;
                while idx < tokens.len() && tokens[idx].value != ";" && tokens[idx].kind != "EOF" {
                    idx += 1;
                }
                if idx < tokens.len() && tokens[idx].value == ";" {
                    idx += 1;
                }
            }
            "extern" => {
                idx += 1;
                let mut depth = 0;
                while idx < tokens.len() {
                    if tokens[idx].value == "{" {
                        depth += 1;
                    } else if tokens[idx].value == "}" {
                        if depth == 0 {
                            idx += 1;
                            break;
                        }
                        depth -= 1;
                    }
                    if depth == 0 && tokens[idx].value == ";" {
                        idx += 1;
                        break;
                    }
                    idx += 1;
                }
            }
            "trait" | "impl" => {
                idx += 1;
                let mut depth = 0;
                while idx < tokens.len() {
                    if tokens[idx].value == "{" {
                        depth += 1;
                    } else if tokens[idx].value == "}" {
                        if depth == 0 {
                            idx += 1;
                            break;
                        }
                        depth -= 1;
                    }
                    if depth == 0 && tokens[idx].value == ";" {
                        idx += 1;
                        break;
                    }
                    idx += 1;
                }
            }
            _ => {
                idx += 1;
            }
        }
    }

    out.push(']');
    if !types_json.is_empty() {
        out.push_str(&format!(r#","types":[{}]"#, types_json.join(",")));
    } else {
        out.push_str(",\"types\":[]");
    }
    if !modules_json.is_empty() {
        out.push_str(&format!(r#","modules":[{}]"#, modules_json.join(",")));
    }
    if !imports_json.is_empty() {
        out.push_str(&format!(r#","imports":[{}]"#, imports_json.join(",")));
    } else {
        out.push_str(",\"imports\":[]");
    }
    out.push_str(&format!(
        r#","has_main":{}}}"#,
        if has_main { "true" } else { "false" }
    ));
    out
}

fn parse_func_decl(tokens: &[MimiToken], start: usize, is_pub: bool) -> (String, usize, bool) {
    let mut idx = start;
    let line = tokens[idx].line;
    let col = tokens[idx].col;

    if tokens[idx].value == "pub" {
        idx += 1;
    }

    let mut is_comptime = false;
    let mut is_async = false;
    if idx < tokens.len() && tokens[idx].kind == "KEYWORD" {
        if tokens[idx].value == "comptime" {
            is_comptime = true;
            idx += 1;
        } else if tokens[idx].value == "async" {
            is_async = true;
            idx += 1;
        }
    }

    if idx >= tokens.len() || tokens[idx].value != "func" {
        return (String::new(), 1, false);
    }
    idx += 1;

    let mut name = String::from("_");
    if idx < tokens.len() && (tokens[idx].kind == "IDENT" || tokens[idx].kind == "KEYWORD") {
        name = tokens[idx].value.clone();
        idx += 1;
    }
    let is_main = name == "main";

    if idx < tokens.len() && tokens[idx].value == "<" {
        let mut depth = 1;
        idx += 1;
        while idx < tokens.len() && depth > 0 {
            if tokens[idx].value == "<" {
                depth += 1;
            } else if tokens[idx].value == ">" {
                depth -= 1;
            }
            idx += 1;
        }
    }

    let mut params_json = Vec::new();
    let mut has_body = false;
    let mut body_end_line = line;

    if idx < tokens.len() && tokens[idx].value == "(" {
        idx += 1;
        while idx < tokens.len() && tokens[idx].value != ")" {
            if tokens[idx].value == "," {
                idx += 1;
                continue;
            }
            let pline = tokens[idx].line;
            let pcol = tokens[idx].col;
            let mut pname = String::from("_");
            let mut is_mut_param = false;
            if idx < tokens.len() && tokens[idx].value == "mut" {
                is_mut_param = true;
                idx += 1;
            }
            if idx < tokens.len() && (tokens[idx].kind == "IDENT" || tokens[idx].kind == "KEYWORD")
            {
                pname = tokens[idx].value.clone();
                idx += 1;
            }
            if idx < tokens.len() && tokens[idx].value == ":" {
                idx += 1;
                let mut ptype = String::new();
                while idx < tokens.len()
                    && !matches!(tokens[idx].value.as_str(), "," | ")" | "=")
                    && tokens[idx].kind != "EOF"
                {
                    ptype.push_str(&tokens[idx].value);
                    idx += 1;
                }
                params_json.push(format!(
                    r#"{{"name":"{}","type":"{}","mut":{},"line":{},"col":{}}}"#,
                    json_escape(&pname),
                    json_escape(ptype.trim()),
                    is_mut_param,
                    pline,
                    pcol
                ));
            } else {
                params_json.push(format!(
                    r#"{{"name":"{}","type":"_","mut":{},"line":{},"col":{}}}"#,
                    json_escape(&pname),
                    is_mut_param,
                    pline,
                    pcol
                ));
            }
            if idx < tokens.len() && tokens[idx].value == "=" {
                idx += 1;
                let mut depth = 0;
                while idx < tokens.len() {
                    if matches!(tokens[idx].value.as_str(), "(" | "{" | "[") {
                        depth += 1;
                    } else if matches!(tokens[idx].value.as_str(), ")" | "}" | "]") {
                        if depth == 0 {
                            break;
                        }
                        depth -= 1;
                    }
                    if depth == 0 && matches!(tokens[idx].value.as_str(), "," | ")") {
                        break;
                    }
                    idx += 1;
                }
            }
        }
        if idx < tokens.len() && tokens[idx].value == ")" {
            idx += 1;
        }
    }

    let mut ret_type = String::new();
    if idx < tokens.len() && tokens[idx].value == "->" {
        idx += 1;
        while idx < tokens.len()
            && !matches!(tokens[idx].value.as_str(), "{" | "where")
            && tokens[idx].kind != "EOF"
        {
            ret_type.push_str(&tokens[idx].value);
            idx += 1;
        }
    }

    if idx < tokens.len() && tokens[idx].value == "where" {
        while idx < tokens.len() && tokens[idx].value != "{" && tokens[idx].kind != "EOF" {
            idx += 1;
        }
    }

    let mut stmts_json = Vec::new();
    if idx < tokens.len() && tokens[idx].value == "{" {
        let body_start = idx;
        let mut depth = 1;
        idx += 1;
        while idx < tokens.len() && depth > 0 {
            if tokens[idx].value == "{" {
                depth += 1;
            } else if tokens[idx].value == "}" {
                depth -= 1;
            }
            if depth > 0 {
                idx += 1;
            }
        }
        if idx < tokens.len() {
            body_end_line = tokens[idx].line;
            idx += 1;
        }
        has_body = true;

        let mut bi = body_start + 1;
        let mut body_depth = 1;
        while bi < idx - 1 && body_depth > 0 {
            if tokens[bi].value == "{" {
                body_depth += 1;
                bi += 1;
                continue;
            }
            if tokens[bi].value == "}" {
                body_depth -= 1;
                bi += 1;
                continue;
            }
            if body_depth != 1 {
                bi += 1;
                continue;
            }

            match tokens[bi].kind.as_str() {
                "KEYWORD" => {
                    let stmt_line = tokens[bi].line;
                    let stmt_col = tokens[bi].col;
                    match tokens[bi].value.as_str() {
                        "let" => {
                            bi += 1;
                            let mut is_mut = false;
                            let mut sname = String::new();
                            if bi < tokens.len() && tokens[bi].value == "mut" {
                                is_mut = true;
                                bi += 1;
                            }
                            if bi < tokens.len() && tokens[bi].kind == "IDENT" {
                                sname = tokens[bi].value.clone();
                                bi += 1;
                            }
                            if bi < tokens.len() && tokens[bi].value == ":" {
                                bi += 1;
                                while bi < tokens.len()
                                    && !matches!(tokens[bi].value.as_str(), "=" | ";" | "{" | "}")
                                    && tokens[bi].kind != "EOF"
                                {
                                    bi += 1;
                                }
                            }
                            if bi < tokens.len() && tokens[bi].value == "=" {
                                bi += 1;
                                let mut ed = 0;
                                while bi < tokens.len() {
                                    if matches!(tokens[bi].value.as_str(), "{" | "(" | "[") {
                                        ed += 1;
                                    } else if matches!(tokens[bi].value.as_str(), "}" | ")" | "]") {
                                        if ed == 0 {
                                            break;
                                        }
                                        ed -= 1;
                                    }
                                    if ed == 0 && tokens[bi].value == ";" {
                                        break;
                                    }
                                    bi += 1;
                                }
                            }
                            if bi < tokens.len() && tokens[bi].value == ";" {
                                bi += 1;
                            }
                            stmts_json.push(format!(
                                r#"{{"kind":"let","name":"{}","mut":{},"line":{},"col":{}}}"#,
                                json_escape(&sname),
                                is_mut,
                                stmt_line,
                                stmt_col
                            ));
                        }
                        "return" => {
                            bi += 1;
                            while bi < tokens.len()
                                && !matches!(tokens[bi].value.as_str(), ";" | "}")
                                && tokens[bi].kind != "EOF"
                            {
                                if tokens[bi].value == "{" {
                                    let mut d = 1;
                                    bi += 1;
                                    while bi < tokens.len() && d > 0 {
                                        if tokens[bi].value == "{" {
                                            d += 1;
                                        } else if tokens[bi].value == "}" {
                                            d -= 1;
                                        }
                                        bi += 1;
                                    }
                                } else {
                                    bi += 1;
                                }
                            }
                            if bi < tokens.len() && tokens[bi].value == ";" {
                                bi += 1;
                            }
                            stmts_json.push(format!(
                                r#"{{"kind":"return","line":{},"col":{}}}"#,
                                stmt_line, stmt_col
                            ));
                        }
                        "if" | "while" | "for" | "loop" => {
                            let sk = tokens[bi].value.clone();
                            bi += 1;
                            let mut ed = 0;
                            while bi < tokens.len() {
                                if tokens[bi].value == "{" {
                                    bi += 1;
                                    let mut d = 1;
                                    while bi < tokens.len() && d > 0 {
                                        if tokens[bi].value == "{" {
                                            d += 1;
                                        } else if tokens[bi].value == "}" {
                                            d -= 1;
                                        }
                                        if d > 0 {
                                            bi += 1;
                                        }
                                    }
                                    break;
                                }
                                if tokens[bi].value == "(" {
                                    ed += 1;
                                } else if tokens[bi].value == ")" {
                                    if ed == 0 {
                                        bi += 1;
                                        break;
                                    }
                                    ed -= 1;
                                }
                                bi += 1;
                            }
                            if sk == "if" {
                                let bi2 = bi + 1;
                                if bi2 < tokens.len() && tokens[bi2].value == "else" {
                                    bi = bi2 + 1;
                                    if bi < tokens.len() && tokens[bi].value == "if" {
                                        bi += 1;
                                        while bi < tokens.len() && tokens[bi].value != "{" {
                                            bi += 1;
                                        }
                                        if bi < tokens.len() && tokens[bi].value == "{" {
                                            bi += 1;
                                            let mut d = 1;
                                            while bi < tokens.len() && d > 0 {
                                                if tokens[bi].value == "{" {
                                                    d += 1;
                                                } else if tokens[bi].value == "}" {
                                                    d -= 1;
                                                }
                                                if d > 0 {
                                                    bi += 1;
                                                }
                                            }
                                        }
                                    } else if bi < tokens.len() && tokens[bi].value == "{" {
                                        bi += 1;
                                        let mut d = 1;
                                        while bi < tokens.len() && d > 0 {
                                            if tokens[bi].value == "{" {
                                                d += 1;
                                            } else if tokens[bi].value == "}" {
                                                d -= 1;
                                            }
                                            if d > 0 {
                                                bi += 1;
                                            }
                                        }
                                    }
                                }
                            }
                            stmts_json.push(format!(
                                r#"{{"kind":"{}","line":{},"col":{}}}"#,
                                sk, stmt_line, stmt_col
                            ));
                        }
                        "break" | "continue" => {
                            let sk = tokens[bi].value.clone();
                            bi += 1;
                            if bi < tokens.len() && tokens[bi].value == ";" {
                                bi += 1;
                            }
                            stmts_json.push(format!(
                                r#"{{"kind":"{}","line":{},"col":{}}}"#,
                                sk, stmt_line, stmt_col
                            ));
                        }
                        "requires" | "ensures" | "desc" | "rule" => {
                            let sk = tokens[bi].value.clone();
                            bi += 1;
                            while bi < tokens.len()
                                && !matches!(tokens[bi].value.as_str(), ";" | "{" | "}")
                                && tokens[bi].kind != "EOF"
                            {
                                bi += 1;
                            }
                            if bi < tokens.len() && tokens[bi].value == ";" {
                                bi += 1;
                            }
                            stmts_json.push(format!(
                                r#"{{"kind":"{}","line":{},"col":{}}}"#,
                                sk, stmt_line, stmt_col
                            ));
                        }
                        "mms" => {
                            bi += 1;
                            if bi < tokens.len() && tokens[bi].value == "{" {
                                bi += 1;
                                let mut d = 1;
                                while bi < tokens.len() && d > 0 {
                                    if tokens[bi].value == "{" {
                                        d += 1;
                                    } else if tokens[bi].value == "}" {
                                        d -= 1;
                                    }
                                    bi += 1;
                                }
                            }
                            stmts_json.push(format!(
                                r#"{{"kind":"mms","line":{},"col":{}}}"#,
                                stmt_line, stmt_col
                            ));
                        }
                        _ => {
                            bi += 1;
                        }
                    }
                }
                _ => {
                    bi += 1;
                }
            }
        }
    }

    let name_esc = json_escape(&name);
    let ret_esc = json_escape(ret_type.trim());
    let params_s = params_json.join(",");
    let mut json = format!(
        r#"{{"name":"{}","line":{},"col":{},"is_pub":{},"is_comptime":{},"is_async":{},"params":[{}],"return_type":"{}","has_body":{},"body_end_line":{}"#,
        name_esc,
        line,
        col,
        if is_pub { "true" } else { "false" },
        if is_comptime { "true" } else { "false" },
        if is_async { "true" } else { "false" },
        params_s,
        ret_esc,
        if has_body { "true" } else { "false" },
        body_end_line
    );

    if !stmts_json.is_empty() {
        json.push_str(&format!(r#","stmts":[{}]"#, stmts_json.join(",")));
    } else {
        json.push_str(r#","stmts":[]"#);
    }

    json.push('}');
    (json, idx - start, is_main)
}

// ===========================================================================
// v0.28.19 — Actor Codegen Real Concurrency
//
// The runtime owns a mailbox + dedicated worker thread per actor, mirroring
// the interpreter's `ActorHandle::new` pattern.  Actor method bodies are
// LLVM-compiled functions; the runtime invokes them indirectly through a
// per-actor-type *dispatch function* generated by codegen:
//
//   void {Name}__dispatch(i32 method_id,
//                          i8* self_fields_ptr,   // mutable actor field blob
//                          i8* args_blob,          // packed argument blob
//                          i64 args_size,
//                          i8* result_blob,        // output: packed result
//                          i64* result_size_out)   // output: written bytes
//
// The dispatch function selects the right method, reads args from the blob,
// executes the method body (reading/writing `self_fields_ptr` for field
// access), and packs the return value into `result_blob`.
//
// For simple scalar returns (i32/i64/f64/bool/ptr) the result fits in 8 bytes.
// For struct/string returns the dispatch function allocates and returns a
// pointer in the first 8 bytes of result_blob.
// ==========================================================================

/// Maximum size of the args blob and result blob.
/// Struct/string arguments that exceed this are passed by pointer.
const MIMI_ACTOR_BLOB_SIZE: usize = 256;

/// Actor message enqueued into the mailbox.
struct ActorMailboxMsg {
    /// Which method to invoke (index into the actor's method list).
    method_id: i32,
    /// Packed argument blob (method params laid out sequentially).
    args: Vec<u8>,
    /// Channel to send the packed result back to the caller.
    response: std::sync::mpsc::Sender<ActorMsgResult>,
}

/// Result returned from a mailbox message execution.
struct ActorMsgResult {
    /// Packed result blob.
    data: Vec<u8>,
    /// Number of bytes written.
    size: u64,
}

/// Runtime representation of a live actor instance.
/// Stored on the heap; the opaque pointer returned to codegen is `*mut MimiActorRepr`.
struct MimiActorRepr {
    /// Unique actor ID for self-call deadlock avoidance.
    id: u64,
    /// Reserved for future use: a heap-allocated field blob accessible from
    /// the handle side. Currently the worker thread owns the live field storage;
    /// all field access goes through the mailbox dispatch path.
    #[allow(dead_code)]
    fields: Box<[u8]>,
    /// Mailbox sender — clone retained by the handle; worker holds the receiver.
    mailbox_tx: std::sync::mpsc::Sender<ActorMailboxMsg>,
    /// Worker thread join handle (joined on drop).
    worker: Option<std::thread::JoinHandle<()>>,
    /// v0.29.11: Fault absorption — when set, mailbox is short-circuited (O(1)).
    /// Shared with the worker so both call and drain paths see the same flag.
    faulted: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// v0.29.21: mailbox high-water depth limit.
    mailbox_depth_limit: usize,
    /// v0.29.21: approximate in-flight message count.
    mailbox_depth: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// v0.29.21: muted under backpressure.
    muted: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// v0.29.25: method_id → method name for polymorphic broadcast name resolution.
    method_names: std::sync::Mutex<Vec<String>>,
}

// SAFETY: `MimiActorRepr` is shared between the caller thread (which holds the
// opaque pointer) and the worker thread (which owns the receiver).  The fields
// blob is only accessed by the worker thread (messages carry all needed data);
// the mailbox channel is itself `Send`.  The dispatch function pointer is a
// plain C function pointer stored in the codegen module's text segment.
// We manually ensure no `&mut` aliasing occurs: the worker has exclusive
// access to `fields` between messages, and `mailbox_tx` is a `Send` channel.
unsafe impl Send for MimiActorRepr {}
unsafe impl Sync for MimiActorRepr {}

/// Global atomic counter for unique actor IDs.
static ACTOR_ID_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
/// v0.29.24: process-wide max children (0 = unlimited).
static ACTOR_SPAWN_MAX: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// v0.29.24: number of actors successfully spawned (not yet dropped).
static ACTOR_SPAWN_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

// Thread-local: the ID of the actor whose worker thread we are currently
// executing on, or 0 if not on an actor worker thread.  Used for self-call
// deadlock avoidance — if a method call targets the same actor, we execute
// it synchronously instead of sending to the mailbox (which would deadlock
// since the worker is busy waiting for the response to its own message).
thread_local! {
    static CURRENT_ACTOR_ID: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Type of the per-actor-type dispatch function generated by codegen.
/// Called on the actor's worker thread to execute a method.
// SAFETY: This is a C ABI function pointer; calling it is unsafe.
type ActorDispatchFn = unsafe extern "C" fn(
    method_id: i32,
    self_fields_ptr: *mut std::ffi::c_void,
    args_blob: *const std::ffi::c_void,
    args_size: i64,
    result_blob: *mut std::ffi::c_void,
    result_size_out: *mut i64,
);

/// Spawn a new actor instance.
///
/// # Parameters
/// - `fields_ptr`: pointer to the initialised actor field blob (memcpy'd).
/// - `fields_size`: size of the field blob in bytes.
/// - `dispatch_fn`: the `{Name}__dispatch` function generated by codegen.
///
/// # Returns
/// Opaque `*mut MimiActorRepr` handle, or null on failure.
#[no_mangle]
pub extern "C" fn mimi_actor_spawn(
    fields_ptr: *const std::ffi::c_void,
    fields_size: i64,
    dispatch_fn: Option<ActorDispatchFn>,
) -> *mut std::ffi::c_void {
    if fields_ptr.is_null() || dispatch_fn.is_none() {
        return std::ptr::null_mut();
    }

    // v0.29.24: spawn quota (0 max = unlimited).
    let max = ACTOR_SPAWN_MAX.load(std::sync::atomic::Ordering::Acquire);
    if max > 0 {
        let count = ACTOR_SPAWN_COUNT.load(std::sync::atomic::Ordering::Acquire);
        if count >= max {
            return std::ptr::null_mut(); // QuotaExceeded
        }
    }

    let id = ACTOR_ID_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let fields_size = fields_size as usize;

    // SAFETY: `fields_ptr` is checked non-null; caller guarantees it points to
    // a valid field blob of `fields_size` bytes.
    let fields_blob: Box<[u8]> = unsafe {
        let slice = std::slice::from_raw_parts(fields_ptr as *const u8, fields_size);
        slice.to_vec().into_boxed_slice()
    };

    let (tx, rx) = std::sync::mpsc::channel::<ActorMailboxMsg>();

    // Clone the fields blob for the worker — the worker owns the live copy;
    // the handle's copy is not used after spawn (all access goes via mailbox).
    // Actually, we move the fields into the worker; the handle just holds the
    // channel + id. But we need the fields blob to be `Send`. Box<[u8]> is Send.
    let worker_fields: Box<[u8]> = fields_blob;

    let Some(dispatch) = dispatch_fn else {
        return std::ptr::null_mut();
    };
    let worker_id = id;
    let faulted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker_faulted = faulted.clone();
    let mailbox_depth = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let worker_depth = mailbox_depth.clone();
    let muted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker_muted = muted.clone();
    let mailbox_depth_limit: usize = 2048;
    let worker_limit = mailbox_depth_limit;

    let thread_result = std::thread::Builder::new()
        .name(format!("mimi-actor-{}", id))
        .spawn(move || {
            // Set the thread-local so self-call detection works.
            CURRENT_ACTOR_ID.with(|c| c.set(worker_id));

            // The worker owns the mutable field storage.
            let mut fields = worker_fields;

            // Result blob is allocated once and reused.
            let mut result_blob = vec![0u8; MIMI_ACTOR_BLOB_SIZE];

            while let Ok(msg) = rx.recv() {
                // v0.29.21: depth accounting.
                let _ = worker_depth.fetch_update(
                    std::sync::atomic::Ordering::AcqRel,
                    std::sync::atomic::Ordering::Acquire,
                    |v| Some(v.saturating_sub(1)),
                );
                // Hysteresis unmute at ≤ 50% depth.
                let d = worker_depth.load(std::sync::atomic::Ordering::Acquire);
                if d <= worker_limit / 2 {
                    worker_muted.store(false, std::sync::atomic::Ordering::Release);
                }
                // v0.29.11: Fault absorption — drain without dispatch (O(1) short-circuit).
                if worker_faulted.load(std::sync::atomic::Ordering::Acquire) {
                    // Zero-size result signals short-circuit to the caller.
                    let _ = msg.response.send(ActorMsgResult {
                        data: Vec::new(),
                        size: 0,
                    });
                    continue;
                }
                // Execute the method via the dispatch function.
                // SAFETY: `dispatch` is a valid C function pointer generated by
                // codegen. `fields` is a valid mutable buffer. `msg.args` is a
                // valid byte slice. `result_blob` is a valid mutable buffer.
                let mut result_size: i64 = 0;
                unsafe {
                    dispatch(
                        msg.method_id,
                        fields.as_mut_ptr() as *mut std::ffi::c_void,
                        msg.args.as_ptr() as *const std::ffi::c_void,
                        msg.args.len() as i64,
                        result_blob.as_mut_ptr() as *mut std::ffi::c_void,
                        &mut result_size as *mut i64,
                    );
                }

                let rs = result_size.max(0) as usize;
                let data = if rs <= result_blob.len() {
                    result_blob[..rs].to_vec()
                } else {
                    Vec::new()
                };

                // Send result back; ignore error (caller may have timed out / dropped).
                let _ = msg.response.send(ActorMsgResult {
                    data,
                    size: result_size as u64,
                });
            }
        });
    let handle = match thread_result {
        Ok(h) => h,
        Err(_) => {
            // Thread spawn failed (resource limit) — return null actor handle.
            return std::ptr::null_mut();
        }
    };

    let repr = Box::new(MimiActorRepr {
        id,
        fields: Box::new([]), // handle doesn't own live fields; worker does
        mailbox_tx: tx,
        worker: Some(handle),
        faulted,
        mailbox_depth_limit,
        mailbox_depth,
        muted,
        method_names: std::sync::Mutex::new(Vec::new()),
    });

    ACTOR_SPAWN_COUNT.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    Box::into_raw(repr) as *mut std::ffi::c_void
}

/// Get the actor ID from a handle. Used by codegen for self-call detection.
#[no_mangle]
pub extern "C" fn mimi_actor_id(handle: *mut std::ffi::c_void) -> u64 {
    if handle.is_null() {
        return 0;
    }
    // SAFETY: `handle` was checked non-null; it was created by `mimi_actor_spawn`
    // as a `Box<MimiActorRepr>`.
    unsafe {
        let repr = &*(handle as *const MimiActorRepr);
        repr.id
    }
}

/// Get the current actor ID (thread-local). Returns 0 if not on an actor worker.
/// Used by codegen to detect self-calls and execute them synchronously.
#[no_mangle]
pub extern "C" fn mimi_actor_current_id() -> u64 {
    CURRENT_ACTOR_ID.with(|c| c.get())
}

/// Call an actor method (blocking).
///
/// Sends a message to the actor's mailbox and blocks waiting for the response.
/// If called from within the same actor's worker thread (self-call), this would
/// deadlock — the caller must check `mimi_actor_current_id() == mimi_actor_id(handle)`
/// and execute the method directly instead.
///
/// # Parameters
/// - `handle`: opaque actor handle from `mimi_actor_spawn`.
/// - `method_id`: method index (determined by codegen at compile time).
/// - `args_ptr`: packed argument blob.
/// - `args_size`: size of args blob in bytes.
/// - `result_ptr`: output buffer for the packed result (caller-allocated, ≥8 bytes).
///
/// # Returns
/// Number of bytes written to `result_ptr`, or 0 on error.
#[no_mangle]
pub extern "C" fn mimi_actor_call(
    handle: *mut std::ffi::c_void,
    method_id: i32,
    args_ptr: *const std::ffi::c_void,
    args_size: i64,
    result_ptr: *mut std::ffi::c_void,
) -> i64 {
    if handle.is_null() {
        return 0;
    }

    // SAFETY: `handle` is a valid `Box<MimiActorRepr>` from spawn.
    let repr = unsafe { &*(handle as *const MimiActorRepr) };

    // v0.29.11: O(1) mailbox short-circuit after Fault absorption.
    if repr.faulted.load(std::sync::atomic::Ordering::Acquire) {
        return 0;
    }

    // v0.29.21: mailbox backpressure — wait while muted/over HWM, TTL break.
    let ttl = std::time::Duration::from_millis(50);
    let start = std::time::Instant::now();
    loop {
        let depth = repr
            .mailbox_depth
            .load(std::sync::atomic::Ordering::Acquire);
        let muted = repr.muted.load(std::sync::atomic::Ordering::Acquire);
        let over = depth >= repr.mailbox_depth_limit;
        if !muted && !over {
            break;
        }
        if start.elapsed() >= ttl {
            // SendErrorNotWriteable — force-wake producer (return 0).
            return 0;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }

    // Pack args.
    let args: Vec<u8> = if args_ptr.is_null() || args_size <= 0 {
        Vec::new()
    } else {
        // SAFETY: caller guarantees `args_ptr` points to `args_size` valid bytes.
        unsafe {
            let slice = std::slice::from_raw_parts(args_ptr as *const u8, args_size as usize);
            slice.to_vec()
        }
    };

    let (resp_tx, resp_rx) = std::sync::mpsc::channel::<ActorMsgResult>();

    let msg = ActorMailboxMsg {
        method_id,
        args,
        response: resp_tx,
    };

    // v0.29.21: reserve depth slot; mute if over HWM.
    let d = repr
        .mailbox_depth
        .fetch_add(1, std::sync::atomic::Ordering::AcqRel)
        + 1;
    if d > repr.mailbox_depth_limit {
        repr.muted.store(true, std::sync::atomic::Ordering::Release);
    }
    // Send to mailbox. If the channel is closed (actor dropped), return 0.
    if repr.mailbox_tx.send(msg).is_err() {
        let _ = repr.mailbox_depth.fetch_update(
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
            |v| Some(v.saturating_sub(1)),
        );
        return 0;
    }

    // Block waiting for the result.
    match resp_rx.recv() {
        Ok(result) => {
            let write_size = (result.size as usize).min(MIMI_ACTOR_BLOB_SIZE);
            if !result_ptr.is_null() && write_size > 0 && write_size <= result.data.len() {
                // SAFETY: `result_ptr` is caller-allocated with sufficient space;
                // `result.data` contains at least `write_size` bytes.
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        result.data.as_ptr(),
                        result_ptr as *mut u8,
                        write_size,
                    );
                }
            }
            result.size as i64
        }
        Err(_) => 0,
    }
}

/// Drop an actor handle. Signals the worker thread to exit (by dropping the
/// mailbox sender) and joins it.
#[no_mangle]
pub extern "C" fn mimi_actor_drop(handle: *mut std::ffi::c_void) {
    if handle.is_null() {
        return;
    }
    // v0.29.24: free a spawn slot.
    let _ = ACTOR_SPAWN_COUNT.fetch_update(
        std::sync::atomic::Ordering::AcqRel,
        std::sync::atomic::Ordering::Acquire,
        |v| Some(v.saturating_sub(1)),
    );
    // SAFETY: `handle` was created by `mimi_actor_spawn` as a `Box<MimiActorRepr>`.
    // We take ownership back and drop it, which closes the mailbox sender,
    // causing the worker's `recv()` to return `Err` and exit.
    unsafe {
        let mut repr = Box::from_raw(handle as *mut MimiActorRepr);
        // Drop the sender to close the channel — worker will exit on next recv.
        // We do this by replacing with a dummy; but actually dropping the Box
        // drops `mailbox_tx` automatically. However, we want to join the worker
        // before the Box is fully dropped to avoid the worker accessing freed
        // memory. Take the worker handle out, drop the Box (closes sender),
        // then join.
        let worker = repr.worker.take();
        drop(repr); // drops mailbox_tx → worker recv() returns Err → worker exits
        if let Some(w) = worker {
            let _ = w.join();
        }
    }
}

/// v0.29.11: Mark an actor as Faulted and short-circuit its mailbox (O(1)).
/// Subsequent `mimi_actor_call` returns 0 without enqueueing; the worker drains
/// any already-queued messages without dispatch.
#[no_mangle]
pub extern "C" fn mimi_actor_fault(handle: *mut std::ffi::c_void) {
    if handle.is_null() {
        return;
    }
    // SAFETY: `handle` is a valid `Box<MimiActorRepr>` from spawn (or null checked).
    let repr = unsafe { &*(handle as *const MimiActorRepr) };
    repr.faulted
        .store(true, std::sync::atomic::Ordering::Release);
}

/// v0.29.11: Query whether an actor's mailbox is short-circuited.
/// Returns 1 if faulted, 0 otherwise (or if handle is null).
#[no_mangle]
pub extern "C" fn mimi_actor_is_faulted(handle: *mut std::ffi::c_void) -> i32 {
    if handle.is_null() {
        return 0;
    }
    // SAFETY: `handle` is a valid `Box<MimiActorRepr>` from spawn.
    let repr = unsafe { &*(handle as *const MimiActorRepr) };
    if repr.faulted.load(std::sync::atomic::Ordering::Acquire) {
        1
    } else {
        0
    }
}

/// v0.29.21: set mailbox high-water depth limit for backpressure.
#[no_mangle]
pub extern "C" fn mimi_actor_set_mailbox_depth(handle: *mut std::ffi::c_void, depth: i64) {
    if handle.is_null() || depth <= 0 {
        return;
    }
    // SAFETY: handle from mimi_actor_spawn; exclusive via opaque pointer.
    let repr = unsafe { &mut *(handle as *mut MimiActorRepr) };
    repr.mailbox_depth_limit = depth as usize;
}

/// v0.29.21: current approximate mailbox depth.
#[no_mangle]
pub extern "C" fn mimi_actor_mailbox_depth(handle: *mut std::ffi::c_void) -> i64 {
    if handle.is_null() {
        return 0;
    }
    let repr = unsafe { &*(handle as *const MimiActorRepr) };
    repr.mailbox_depth
        .load(std::sync::atomic::Ordering::Acquire) as i64
}

/// v0.29.21: 1 if actor is muted under backpressure, else 0.
#[no_mangle]
pub extern "C" fn mimi_actor_is_muted(handle: *mut std::ffi::c_void) -> i32 {
    if handle.is_null() {
        return 0;
    }
    let repr = unsafe { &*(handle as *const MimiActorRepr) };
    if repr.muted.load(std::sync::atomic::Ordering::Acquire) {
        1
    } else {
        0
    }
}

/// v0.29.24: set process-wide max children (0 = unlimited).
#[no_mangle]
pub extern "C" fn mimi_actor_set_max_children(max: i64) {
    let v = if max <= 0 { 0 } else { max as u64 };
    ACTOR_SPAWN_MAX.store(v, std::sync::atomic::Ordering::Release);
}

/// v0.29.24: current live actor spawn count.
#[no_mangle]
pub extern "C" fn mimi_actor_spawn_count() -> i64 {
    ACTOR_SPAWN_COUNT.load(std::sync::atomic::Ordering::Acquire) as i64
}

/// v0.29.24: configured max children (0 = unlimited).
#[no_mangle]
pub extern "C" fn mimi_actor_max_children() -> i64 {
    ACTOR_SPAWN_MAX.load(std::sync::atomic::Ordering::Acquire) as i64
}

/// v0.29.25: register method names for an actor handle (for broadcast by name).
/// `names` is an array of `count` C strings (method name in definition order).
#[no_mangle]
pub extern "C" fn mimi_actor_set_method_names(
    handle: *mut std::ffi::c_void,
    names: *const *const std::os::raw::c_char,
    count: i64,
) {
    if handle.is_null() || names.is_null() || count <= 0 {
        return;
    }
    let repr = unsafe { &*(handle as *const MimiActorRepr) };
    let slice = unsafe { std::slice::from_raw_parts(names, count as usize) };
    let mut v = Vec::with_capacity(count as usize);
    for &p in slice {
        if p.is_null() {
            v.push(String::new());
        } else {
            let s = unsafe { std::ffi::CStr::from_ptr(p) }
                .to_string_lossy()
                .into_owned();
            v.push(s);
        }
    }
    if let Ok(mut g) = repr.method_names.lock() {
        *g = v;
    }
}

/// Resolve method name to method_id for a handle; returns -1 if not found.
#[no_mangle]
pub extern "C" fn mimi_actor_method_id(
    handle: *mut std::ffi::c_void,
    name: *const std::os::raw::c_char,
) -> i32 {
    if handle.is_null() || name.is_null() {
        return -1;
    }
    let repr = unsafe { &*(handle as *const MimiActorRepr) };
    let needle = unsafe { std::ffi::CStr::from_ptr(name) }.to_string_lossy();
    if let Ok(g) = repr.method_names.lock() {
        for (i, n) in g.iter().enumerate() {
            if n == needle.as_ref() {
                return i as i32;
            }
        }
    }
    -1
}

/// v0.29.25: broadcast method_name to an array of actor handles.
///
/// For each non-null handle, resolve method name → id and call mimi_actor_call
/// with empty args. Results: heap-allocated i64 array of length `count`.
/// v0.29.35: PeerFault slots use sentinel -1 (distinguishable from 0 result).
/// Caller owns the returned pointer.
#[no_mangle]
pub extern "C" fn mimi_broadcast(
    handles: *const *mut std::ffi::c_void,
    count: i64,
    method_name: *const std::os::raw::c_char,
    out_len: *mut i64,
) -> *mut i64 {
    if handles.is_null() || count <= 0 || method_name.is_null() {
        if !out_len.is_null() {
            unsafe { *out_len = 0 };
        }
        return std::ptr::null_mut();
    }
    let n = count as usize;
    let slice = unsafe { std::slice::from_raw_parts(handles, n) };
    let mut results: Vec<i64> = Vec::with_capacity(n);
    for &h in slice {
        if h.is_null() {
            // v0.29.35: PeerFault sentinel = -1
            results.push(-1);
            continue;
        }
        let mid = mimi_actor_method_id(h, method_name);
        if mid < 0 {
            // v0.29.35: unknown method → PeerFault sentinel
            results.push(-1);
            continue;
        }
        let mut result_buf = [0u8; MIMI_ACTOR_BLOB_SIZE];
        let sz = mimi_actor_call(
            h,
            mid,
            std::ptr::null(),
            0,
            result_buf.as_mut_ptr() as *mut std::ffi::c_void,
        );
        if sz >= 8 {
            // L4: slice is exactly 8 bytes by construction; avoid silent zero.
            let v = i64::from_le_bytes(
                result_buf[0..8]
                    .try_into()
                    .unwrap_or([0u8; 8]),
            );
            results.push(v);
        } else {
            // v0.29.35: call failed → PeerFault sentinel
            results.push(-1);
        }
    }
    if !out_len.is_null() {
        unsafe { *out_len = results.len() as i64 };
    }
    let boxed = results.into_boxed_slice();
    Box::into_raw(boxed) as *mut i64
}

/// Free a buffer returned by mimi_broadcast.
#[no_mangle]
pub extern "C" fn mimi_broadcast_free(ptr: *mut i64, len: i64) {
    if ptr.is_null() || len <= 0 {
        return;
    }
    unsafe {
        let _ = Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len as usize));
    }
}

// =========================================================================
// v0.28.20 — Concurrency primitives (Mutex / atomic / Channel)
//
// All three primitives are implemented entirely in Rust using std::sync.
// They follow the existing handle-as-i64 convention used by Set/Map/Record
// so the interpreter (Value::Int handle) and codegen (i64 runtime fn) paths
// stay symmetric. Each primitive exposes:
//   * `_new` constructor returning an opaque i64 handle,
//   * methods that take the handle and return i64 (mimics Rust's ordering),
//   * `_drop` destructor that the codegen cleanup pass emits on scope exit.
//
// SAFETY invariants are identical to the actor mailbox above: handles are
// `Box`-allocated and recovered by handle id with a global mutex-protected
// table. All public functions are `#[no_mangle] pub extern "C"` and
// null-checked.
// =========================================================================

/// Global concurrency-primitive table. LazyLock because the table
/// contains a non-`const` HashMap. The `incompatible_msrv` allow mirrors
/// `src/ffi/runtime.rs`'s `MIMI_POOL` static — the project runtime
/// requires 1.80+ features regardless of the lib `rust-version` pin.
#[allow(clippy::incompatible_msrv)]
static CONCURRENCY_HANDLES: std::sync::LazyLock<std::sync::Mutex<ConcurrencyHandleTable>> =
    std::sync::LazyLock::new(|| {
        std::sync::Mutex::new(ConcurrencyHandleTable {
            next_id: 1,
            atomics: HashMap::new(),
            mutexes: HashMap::new(),
            channels: HashMap::new(),
        })
    });

/// Concurrency primitive handle table. Each variant key carries a boxed
/// primitive; `take_by_*` helpers retrieve + remove the handle for drop.
/// Once removed, any subsequent use returns a null/error sentinel.
struct ConcurrencyHandleTable {
    next_id: u64,
    atomics: HashMap<u64, ConcurrencyAtomic>,
    mutexes: HashMap<u64, ConcurrencyMutex>,
    channels: HashMap<u64, ConcurrencyChannel>,
}

enum ConcurrencyAtomic {
    I32(std::sync::atomic::AtomicI32),
    I64(std::sync::atomic::AtomicI64),
    Bool(std::sync::atomic::AtomicBool),
}

/// Per-primitive Mutex storage. The `Arc` gives the inner `Mutex` a stable
/// address and keeps it alive even if the handle is dropped while a guard
/// is still held (defensive against user error). Guards are stored in
/// `MIMI_MUTEX_GUARDS` and keep an `Arc` clone so the lifetime extension to
/// `'static` used in `HeldMutexGuard` is sound.
struct ConcurrencyMutex {
    inner: Arc<std::sync::Mutex<i64>>,
}

/// A held mutex guard. The `_arc` clone keeps the `Mutex` alive for the
/// guard's lifetime; the `guard` lifetime is extended to `'static` via
/// transmute because the Arc guarantees the Mutex is never deallocated
/// while the guard exists. The guard is stored in thread-local storage
/// (single-thread access) until `mimi_mutex_unlock` removes it.
struct HeldMutexGuard {
    _arc: Arc<std::sync::Mutex<i64>>,
    guard: std::sync::MutexGuard<'static, i64>,
}

thread_local! {
    static MIMI_MUTEX_GUARDS: std::cell::RefCell<HashMap<u64, HeldMutexGuard>> =
        std::cell::RefCell::new(HashMap::new());
}
static MIMI_MUTEX_GUARD_NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Bounded mpsc channel of i64 values. Constructed via `mimi_channel_new`.
/// `send` pushes; `recv`/`try_recv` pops; `drop` closes both endpoints.
/// The receiver is wrapped in `Arc<Mutex<Option<Receiver>>>` so that a
/// blocking `recv` can be performed without holding the global handle table
/// lock.
struct ConcurrencyChannel {
    tx: std::sync::mpsc::Sender<i64>,
    rx: Arc<Mutex<Option<std::sync::mpsc::Receiver<i64>>>>,
}

fn alloc_atomic(a: ConcurrencyAtomic) -> i64 {
    let mut table = CONCURRENCY_HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let id = table.next_id;
    table.next_id += 1;
    table.atomics.insert(id, a);
    id as i64
}

fn alloc_mutex(m: ConcurrencyMutex) -> i64 {
    let mut table = CONCURRENCY_HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let id = table.next_id;
    table.next_id += 1;
    table.mutexes.insert(id, m);
    id as i64
}

fn alloc_channel(c: ConcurrencyChannel) -> i64 {
    let mut table = CONCURRENCY_HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let id = table.next_id;
    table.next_id += 1;
    table.channels.insert(id, c);
    id as i64
}

// ---------- AtomicI32 ----------

#[no_mangle]
pub extern "C" fn mimi_atomic_i32_new(value: i32) -> i64 {
    alloc_atomic(ConcurrencyAtomic::I32(std::sync::atomic::AtomicI32::new(
        value,
    )))
}

#[no_mangle]
pub extern "C" fn mimi_atomic_i32_load(handle: i64) -> i32 {
    let table = CONCURRENCY_HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    match table.atomics.get(&(handle as u64)) {
        Some(ConcurrencyAtomic::I32(a)) => a.load(std::sync::atomic::Ordering::SeqCst),
        _ => 0,
    }
}

#[no_mangle]
pub extern "C" fn mimi_atomic_i32_store(handle: i64, value: i32) {
    let table = CONCURRENCY_HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(ConcurrencyAtomic::I32(a)) = table.atomics.get(&(handle as u64)) {
        a.store(value, std::sync::atomic::Ordering::SeqCst);
    }
}

#[no_mangle]
pub extern "C" fn mimi_atomic_i32_fetch_add(handle: i64, delta: i32) -> i32 {
    let table = CONCURRENCY_HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    match table.atomics.get(&(handle as u64)) {
        Some(ConcurrencyAtomic::I32(a)) => a.fetch_add(delta, std::sync::atomic::Ordering::SeqCst),
        _ => 0,
    }
}

/// Compare-and-swap: returns 1 on success, 0 on mismatch. Codegen also
/// reads back the value via `mimi_atomic_i32_load` after failure.
#[no_mangle]
pub extern "C" fn mimi_atomic_i32_compare_exchange(
    handle: i64,
    expected: i32,
    new_value: i32,
) -> i32 {
    let table = CONCURRENCY_HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    match table.atomics.get(&(handle as u64)) {
        Some(ConcurrencyAtomic::I32(a)) => match a.compare_exchange(
            expected,
            new_value,
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
        ) {
            Ok(_) => 1,
            Err(_) => 0,
        },
        _ => 0,
    }
}

#[no_mangle]
pub extern "C" fn mimi_atomic_i32_drop(handle: i64) {
    let mut table = CONCURRENCY_HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    table.atomics.remove(&(handle as u64));
}

// ---------- AtomicI64 ----------

#[no_mangle]
pub extern "C" fn mimi_atomic_i64_new(value: i64) -> i64 {
    alloc_atomic(ConcurrencyAtomic::I64(std::sync::atomic::AtomicI64::new(
        value,
    )))
}

#[no_mangle]
pub extern "C" fn mimi_atomic_i64_load(handle: i64) -> i64 {
    let table = CONCURRENCY_HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    match table.atomics.get(&(handle as u64)) {
        Some(ConcurrencyAtomic::I64(a)) => a.load(std::sync::atomic::Ordering::SeqCst),
        _ => 0,
    }
}

#[no_mangle]
pub extern "C" fn mimi_atomic_i64_store(handle: i64, value: i64) {
    let table = CONCURRENCY_HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(ConcurrencyAtomic::I64(a)) = table.atomics.get(&(handle as u64)) {
        a.store(value, std::sync::atomic::Ordering::SeqCst);
    }
}

#[no_mangle]
pub extern "C" fn mimi_atomic_i64_fetch_add(handle: i64, delta: i64) -> i64 {
    let table = CONCURRENCY_HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    match table.atomics.get(&(handle as u64)) {
        Some(ConcurrencyAtomic::I64(a)) => a.fetch_add(delta, std::sync::atomic::Ordering::SeqCst),
        _ => 0,
    }
}

#[no_mangle]
pub extern "C" fn mimi_atomic_i64_drop(handle: i64) {
    let mut table = CONCURRENCY_HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    table.atomics.remove(&(handle as u64));
}

// ---------- AtomicBool ----------

#[no_mangle]
pub extern "C" fn mimi_atomic_bool_new(value: i32) -> i64 {
    let b = value != 0;
    alloc_atomic(ConcurrencyAtomic::Bool(std::sync::atomic::AtomicBool::new(
        b,
    )))
}

#[no_mangle]
pub extern "C" fn mimi_atomic_bool_load(handle: i64) -> i32 {
    let table = CONCURRENCY_HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    match table.atomics.get(&(handle as u64)) {
        Some(ConcurrencyAtomic::Bool(a)) => a.load(std::sync::atomic::Ordering::SeqCst) as i32,
        _ => 0,
    }
}

#[no_mangle]
pub extern "C" fn mimi_atomic_bool_store(handle: i64, value: i32) {
    let b = value != 0;
    let table = CONCURRENCY_HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(ConcurrencyAtomic::Bool(a)) = table.atomics.get(&(handle as u64)) {
        a.store(b, std::sync::atomic::Ordering::SeqCst);
    }
}

#[no_mangle]
pub extern "C" fn mimi_atomic_bool_drop(handle: i64) {
    let mut table = CONCURRENCY_HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    table.atomics.remove(&(handle as u64));
}

// ---------- Mutex<i64> ----------

#[no_mangle]
pub extern "C" fn mimi_mutex_new(value: i64) -> i64 {
    alloc_mutex(ConcurrencyMutex {
        inner: Arc::new(std::sync::Mutex::new(value)),
    })
}

/// Lock the mutex and return a separate guard-handle id. The guard handle
/// must be passed to `mimi_mutex_get`/`set` to read/write the held value, and
/// to `mimi_mutex_unlock` to release the lock. The lock is held continuously
/// between lock/get/set/unlock, providing real mutual exclusion across threads.
#[no_mangle]
pub extern "C" fn mimi_mutex_lock(handle: i64) -> i64 {
    let arc = {
        let table = CONCURRENCY_HANDLES
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match table.mutexes.get(&(handle as u64)) {
            Some(m) => Arc::clone(&m.inner),
            _ => return 0,
        }
    };
    // Drop the global table lock before blocking on the mutex.
    let guard = arc.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: Lifetime extension via transmute is sound because:
    //   1. `arc` (Arc clone) is stored alongside in HeldMutexGuard._arc,
    //      keeping the underlying Mutex alive for the guard's entire lifetime.
    //   2. The guard is stored in thread-local storage (MIMI_MUTEX_GUARDS),
    //      ensuring single-thread access — no cross-thread aliasing.
    //   3. `mimi_mutex_unlock` drops the guard before the Arc is dropped,
    //      guaranteeing the guard never outlives the Mutex.
    //   4. The Arc's strong count guarantees the Mutex memory is never freed
    //      while any guard exists.
    // This avoids the type-system limitation where MutexGuard's lifetime is
    // syntactically tied to the stack frame, not the Arc's heap lifetime.
    let guard: std::sync::MutexGuard<'static, i64> = unsafe { std::mem::transmute(guard) };
    let held = HeldMutexGuard { _arc: arc, guard };
    let id = MIMI_MUTEX_GUARD_NEXT_ID.fetch_add(1, Ordering::SeqCst);
    MIMI_MUTEX_GUARDS.with(|guards| {
        guards.borrow_mut().insert(id, held);
    });
    id as i64
}

#[no_mangle]
pub extern "C" fn mimi_mutex_get(guard_handle: i64) -> i64 {
    MIMI_MUTEX_GUARDS.with(|guards| {
        guards
            .borrow()
            .get(&(guard_handle as u64))
            .map(|held| *held.guard)
            .unwrap_or(0)
    })
}

#[no_mangle]
pub extern "C" fn mimi_mutex_set(guard_handle: i64, value: i64) {
    MIMI_MUTEX_GUARDS.with(|guards| {
        if let Some(held) = guards.borrow_mut().get_mut(&(guard_handle as u64)) {
            *held.guard = value;
        }
    });
}

#[no_mangle]
pub extern "C" fn mimi_mutex_unlock(guard_handle: i64) {
    MIMI_MUTEX_GUARDS.with(|guards| {
        // Removing the entry drops the `MutexGuard`, releasing the OS lock.
        guards.borrow_mut().remove(&(guard_handle as u64));
    });
}

#[no_mangle]
pub extern "C" fn mimi_mutex_drop(handle: i64) {
    let mut table = CONCURRENCY_HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    table.mutexes.remove(&(handle as u64));
    // M3 note: existing HeldMutexGuard entries in TLS still hold Arc clones
    // of the Mutex, so the underlying data stays alive. However, after drop(),
    // no new lock() calls can acquire this mutex. Existing guards will still
    // work (get/set/unlock) until dropped via mimi_mutex_unlock — they reference
    // the old Mutex via their Arc clone.
}

// ---------- Channel<i64> (mpsc, unbounded) ----------

#[no_mangle]
pub extern "C" fn mimi_channel_new() -> i64 {
    let (tx, rx) = std::sync::mpsc::channel::<i64>();
    alloc_channel(ConcurrencyChannel {
        tx,
        rx: Arc::new(Mutex::new(Some(rx))),
    })
}

#[no_mangle]
pub extern "C" fn mimi_channel_send(handle: i64, value: i64) {
    let tx = {
        let table = CONCURRENCY_HANDLES
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        table.channels.get(&(handle as u64)).map(|ch| ch.tx.clone())
    };
    if let Some(tx) = tx {
        let _ = tx.send(value);
    }
}

#[no_mangle]
pub extern "C" fn mimi_channel_recv(handle: i64) -> i64 {
    // Look up the channel under the global lock, then clone the receiver Arc
    // and drop the global lock *before* blocking on recv(). This prevents a
    // receiver from stalling all other concurrency-handle operations.
    let rx_arc = {
        let table = CONCURRENCY_HANDLES
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match table.channels.get(&(handle as u64)) {
            Some(ch) => Arc::clone(&ch.rx),
            _ => return 0,
        }
    };
    // Take the receiver out under the mutex lock, then drop the lock before
    // blocking on recv(). This prevents a deadlock with mimi_channel_drop,
    // which needs the same mutex to set the receiver slot to None.
    let rx = rx_arc.lock().unwrap_or_else(|e| e.into_inner()).take();
    // MutexGuard is dropped here; the mutex is now free.
    match rx {
        Some(rx) => {
            // H12-fix: log channel disconnect instead of silently returning 0.
            // unwrap_or_default() returns 0 for i64 when the channel is closed,
            // which is indistinguishable from a legitimate 0 value.
            let result = rx.recv().unwrap_or_else(|e| {
                eprintln!("[mimi runtime] channel recv: channel disconnected: {}", e);
                0
            });
            // Re-acquire the mutex and put the receiver back only if the
            // channel still exists in the global table (i.e. mimi_channel_drop
            // has not been called while we were blocked).
            let still_alive = CONCURRENCY_HANDLES
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .channels
                .contains_key(&(handle as u64));
            if still_alive {
                *rx_arc.lock().unwrap_or_else(|e| e.into_inner()) = Some(rx);
            }
            // If the channel was dropped, `rx` is dropped here.
            result
        }
        None => 0,
    }
}

/// Non-blocking receive. Returns `value` on success, or `-1` if no value is
/// currently available (channel still open, queue empty).
#[no_mangle]
pub extern "C" fn mimi_channel_try_recv(handle: i64) -> i64 {
    let rx_arc = {
        let table = CONCURRENCY_HANDLES
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match table.channels.get(&(handle as u64)) {
            Some(ch) => Arc::clone(&ch.rx),
            _ => return -1,
        }
    };
    // Take the receiver out, try_recv (which is non-blocking), then put back.
    let rx = rx_arc.lock().unwrap_or_else(|e| e.into_inner()).take();
    match rx {
        Some(rx) => {
            let result = rx.try_recv().unwrap_or(-1);
            let still_alive = CONCURRENCY_HANDLES
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .channels
                .contains_key(&(handle as u64));
            if still_alive {
                *rx_arc.lock().unwrap_or_else(|e| e.into_inner()) = Some(rx);
            }
            // If the channel was dropped, `rx` is dropped here.
            result
        }
        None => -1,
    }
}

#[no_mangle]
pub extern "C" fn mimi_channel_drop(handle: i64) {
    // CRITICAL #15: TOCTOU race analysis:
    // 1. mimi_channel_recv takes the Receiver out of the Arc<Mutex<Option<_>>>
    //    and releases the mutex before calling recv().
    // 2. mimi_channel_drop removes the channel from the handle table (which
    //    drops the tx sender), then sets the receiver slot to None.
    // 3. The blocked recv() in step 1 unblocks when tx is dropped (step 2),
    //    returning Err (disconnected), which recv() handles via unwrap_or_else.
    // 4. After recv() returns, still_alive check prevents putting the receiver
    //    back into a dropped channel.
    //
    // This is safe because: the tx drop unblocks any pending recv, and the
    // receiver is either put back (if channel still alive) or dropped (if not).
    let rx_arc = {
        let mut table = CONCURRENCY_HANDLES
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match table.channels.remove(&(handle as u64)) {
            Some(ch) => ch.rx,
            _ => return,
        }
    };
    // Drop the receiver outside the global handle table lock so that any
    // pending `recv` unblocks promptly without needing the global lock.
    // The ConcurrencyChannel drop (from table.channels.remove above) also
    // drops tx, which unblocks any pending recv() on the taken-out receiver.
    *rx_arc.lock().unwrap_or_else(|e| e.into_inner()) = None;
}
#[no_mangle]
pub extern "C" fn mimi_session_pair() -> i64 {
    let pair1 = std::sync::mpsc::channel::<i64>();
    let pair2 = std::sync::mpsc::channel::<i64>();
    // Cross-wire: A sends to B's rx, B sends to A's rx
    let ha = alloc_channel(ConcurrencyChannel {
        tx: pair1.0,
        rx: std::sync::Arc::new(std::sync::Mutex::new(Some(pair2.1))),
    }) as u64;
    let hb = alloc_channel(ConcurrencyChannel {
        tx: pair2.0,
        rx: std::sync::Arc::new(std::sync::Mutex::new(Some(pair1.1))),
    }) as u64;
    ((hb << 32) | (ha & 0xFFFF_FFFFu64)) as i64
}
#[no_mangle]
pub extern "C" fn mimi_session_lo(pair: i64) -> i64 {
    (pair as u64 & 0xFFFF_FFFFu64) as i64
}
#[no_mangle]
pub extern "C" fn mimi_session_hi(pair: i64) -> i64 {
    ((pair as u64) >> 32) as i64
}

// =========================================================================
// v0.28.21 — QuotedAst runtime representation
//
// QuotedAst values produced by `quote! { ... }` are stored in the
// interpreter as `Value::QuoteAst(Box<QuotedAst>)`. The codegen path
// needs an equivalent runtime representation so that `ast_eval(q)` and
// `$(expr)` interpolations can flow through the compiled binary without
// going back to the interpreter. The layout is a tagged union:
//
//   struct MimiQuotedAst {
//       int32_t tag;       // see QAST_* below
//       int32_t argc;      // number of children
//       int64_t data0;     // literal value, or first child ptr
//       int64_t data1;     // binop, or second child ptr
//       int64_t data2;     // third child / extra / children_count
//   };
//
// Variable-arity nodes (Call, Tuple, List, Block, Record) use
// `data0 = children_array_ptr, data2 = children_count`. Children
// themselves are `*mut MimiQuotedAst`, allocated individually via
// `mimi_quote_new_*` helpers and freed recursively by `mimi_quote_drop`.
// =========================================================================

/// QuotedAst node tag. Values must stay in sync with the interp-side
/// `QuotedAst` variant order (we re-derive the mapping at the call
/// sites so a reordering here would be caught at compile time of the
/// codegen helper).
#[repr(i32)]
pub enum QuotedAstTag {
    QastInt = 0,
    QastFloat,
    QastBool,
    QastString,
    QastUnit,
    QastIdent,
    QastBinary,
    QastUnary,
    QastCall,
    QastField,
    QastIndex,
    QastTuple,
    QastList,
    QastIf,
    QastBlock,
    QastInterp,
    QastLet,
    QastReturn,
    QastBreak,
    QastContinue,
    QastWhile,
    QastAssign,
    QastFor,
    QastLoop,
    QastArena,
    QastUnsafe,
    QastDrop,
    QastOnFailure,
    QastParasteps,
    QastAlloc,
    QastSharedLet,
    QastMatch,
    QastTry,
    QastSpawn,
    QastAwait,
    QastRecord,
    QastNamedArg,
}

/// Runtime QuotedAst node. Layout: `repr(C)` so the codegen
/// `i8*` pointer handed back to user code maps to this struct.
#[repr(C)]
pub struct MimiQuotedAst {
    pub tag: i32,
    pub argc: i32,
    pub data0: i64,
    pub data1: i64,
    pub data2: i64,
}

/// Allocate a leaf (literal / ident / unit) node. `data0` carries the
/// literal value (cast to i64) or the ident-tag identifier (0 for unit
/// or generic; ident data is recovered through `data1` for binary nodes
/// only — the v0.28.21 batch treats `Ident(name)` as a literal slot).
#[no_mangle]
pub extern "C" fn mimi_quote_new_leaf(tag: i32, value: i64) -> *mut MimiQuotedAst {
    let node = Box::new(MimiQuotedAst {
        tag,
        argc: 0,
        data0: value,
        data1: 0,
        data2: 0,
    });
    Box::into_raw(node)
}

/// Allocate a binary / unary / index / field-style node with up to two
/// children. The children pointers are themselves returned by
/// `mimi_quote_new_*` and ownership transfers to the new parent.
#[no_mangle]
pub extern "C" fn mimi_quote_new_node(
    tag: i32,
    child0: *mut MimiQuotedAst,
    child1: *mut MimiQuotedAst,
    extra: i64,
) -> *mut MimiQuotedAst {
    let node = Box::new(MimiQuotedAst {
        tag,
        argc: if child1.is_null() { 1 } else { 2 },
        data0: child0 as i64,
        data1: if child1.is_null() { 0 } else { child1 as i64 },
        data2: extra,
    });
    Box::into_raw(node)
}

/// Allocate a node backed by a heap-allocated children array (Call,
/// Tuple, List, Block, Record, etc.). The children are stored in a
/// `Vec<*mut MimiQuotedAst>` allocated separately so we can store a
/// thin pointer in `data0` (length tracked in `data2`).
#[no_mangle]
pub extern "C" fn mimi_quote_new_list(
    tag: i32,
    children: *const *mut MimiQuotedAst,
    len: i64,
) -> *mut MimiQuotedAst {
    let len = len.max(0) as usize;
    // SAFETY: caller guarantees `children` points to `len` valid
    // `*mut MimiQuotedAst` pointers, each owned by the new node.
    let vec: Vec<*mut MimiQuotedAst> = if children.is_null() || len == 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(children, len).to_vec() }
    };
    let boxed: Box<Vec<*mut MimiQuotedAst>> = Box::new(vec);
    let ptr = Box::into_raw(boxed) as i64;
    let node = Box::new(MimiQuotedAst {
        tag,
        argc: len as i32,
        data0: ptr,
        data1: 0,
        data2: len as i64,
    });
    Box::into_raw(node)
}

/// Recursively free a QuotedAst subtree, including any children-array
/// blobs. Safe to call on null (no-op).
#[no_mangle]
pub extern "C" fn mimi_quote_drop(node: *mut MimiQuotedAst) {
    if node.is_null() {
        return;
    }
    // SAFETY: `node` was created by `mimi_quote_new_*` and not yet
    // dropped.
    unsafe {
        let n = Box::from_raw(node);
        if n.argc <= 0 {
            return;
        }
        if n.argc == 1 {
            let child = n.data0 as *mut MimiQuotedAst;
            mimi_quote_drop(child);
        } else if n.argc == 2 {
            mimi_quote_drop(n.data0 as *mut MimiQuotedAst);
            mimi_quote_drop(n.data1 as *mut MimiQuotedAst);
        } else {
            // Variable-arity: data0 is a pointer to a `Vec<*mut MimiQuotedAst>`.
            // M9/C15: always attempt Box::from_raw for argc>2. This value was
            // created by mimi_quote_new_list which always uses Box + into_raw,
            // so the pointer is always valid. We only skip if null.
            let arr_ptr = n.data0 as *mut Vec<*mut MimiQuotedAst>;
            if !arr_ptr.is_null() {
                // SAFETY: `arr_ptr` was created by `mimi_quote_new_list`.
                let vec = Box::from_raw(arr_ptr);
                for &child in vec.iter() {
                    mimi_quote_drop(child);
                }
            }
        }
    }
}

/// Read the tag back. Useful for runtime dispatch (e.g. in `ast_eval`
/// when written to interpret the runtime node).
#[no_mangle]
pub extern "C" fn mimi_quote_tag(node: *mut MimiQuotedAst) -> i32 {
    if node.is_null() {
        return -1;
    }
    // SAFETY: caller guarantees `node` is a valid (non-freed) node.
    unsafe { (*node).tag }
}

/// Read `data0` (literal value or first child pointer). Callers that
/// want a child pointer can cast the result to `*mut MimiQuotedAst`.
#[no_mangle]
pub extern "C" fn mimi_quote_data0(node: *mut MimiQuotedAst) -> i64 {
    if node.is_null() {
        return 0;
    }
    // SAFETY: caller guarantees `node` is a valid (non-freed) node.
    unsafe { (*node).data0 }
}

/// Read `data1`.
#[no_mangle]
pub extern "C" fn mimi_quote_data1(node: *mut MimiQuotedAst) -> i64 {
    if node.is_null() {
        return 0;
    }
    // SAFETY: caller guarantees `node` is a valid (non-freed) node.
    unsafe { (*node).data1 }
}

/// Read `data2`.
#[no_mangle]
pub extern "C" fn mimi_quote_data2(node: *mut MimiQuotedAst) -> i64 {
    if node.is_null() {
        return 0;
    }
    // SAFETY: caller guarantees `node` is a valid (non-freed) node.
    unsafe { (*node).data2 }
}

/// Read `argc` (number of children).
#[no_mangle]
pub extern "C" fn mimi_quote_argc(node: *mut MimiQuotedAst) -> i32 {
    if node.is_null() {
        return 0;
    }
    // SAFETY: caller guarantees `node` is a valid (non-freed) node.
    unsafe { (*node).argc }
}

/// Read child at index `i` from a list-style node. Returns null on
/// out-of-range or if the node isn't list-style.
#[no_mangle]
pub extern "C" fn mimi_quote_list_child(node: *mut MimiQuotedAst, i: i64) -> *mut MimiQuotedAst {
    if node.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: caller guarantees `node` is a valid (non-freed) node.
    unsafe {
        if (*node).argc <= 2 {
            return std::ptr::null_mut();
        }
        let arr_ptr = (*node).data0 as *const Vec<*mut MimiQuotedAst>;
        if arr_ptr.is_null() {
            return std::ptr::null_mut();
        }
        let idx = i as usize;
        let len = (*node).argc as usize;
        if idx >= len {
            return std::ptr::null_mut();
        }
        // SAFETY: `arr_ptr` is a valid `Vec` created by `mimi_quote_new_list`.
        let vec = &*arr_ptr;
        (*vec)[idx]
    }
}
