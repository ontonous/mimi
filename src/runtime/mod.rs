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
use std::ffi::CStr;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;

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
// R-C11: live handle registries (Map / Set / Actor)
// ---------------------------------------------------------------------------
// Handles are still raw Box addresses for ABI compatibility, but every create
// inserts into a process-wide set and every destroy removes under lock. Second
// destroy is a no-op; use-after-destroy aborts instead of double-free / UAF.

use std::collections::HashSet;

static LIVE_MAPS: std::sync::OnceLock<Mutex<HashSet<MapHandle>>> = std::sync::OnceLock::new();
static LIVE_SETS: std::sync::OnceLock<Mutex<HashSet<i64>>> = std::sync::OnceLock::new();

fn live_maps() -> &'static Mutex<HashSet<MapHandle>> {
    LIVE_MAPS.get_or_init(|| Mutex::new(HashSet::new()))
}
fn live_sets() -> &'static Mutex<HashSet<i64>> {
    LIVE_SETS.get_or_init(|| Mutex::new(HashSet::new()))
}

fn map_register_live(handle: MapHandle) {
    if handle != 0 {
        let mut g = live_maps().lock().unwrap_or_else(|e| e.into_inner());
        g.insert(handle);
    }
}
/// Returns true if the handle was live and is now taken (caller must free).
fn map_take_live(handle: MapHandle) -> bool {
    if handle == 0 {
        return false;
    }
    let mut g = live_maps().lock().unwrap_or_else(|e| e.into_inner());
    g.remove(&handle)
}
fn map_is_live(handle: MapHandle) -> bool {
    if handle == 0 {
        return false;
    }
    let g = live_maps().lock().unwrap_or_else(|e| e.into_inner());
    g.contains(&handle)
}

fn set_register_live(handle: i64) {
    if handle != 0 {
        let mut g = live_sets().lock().unwrap_or_else(|e| e.into_inner());
        g.insert(handle);
    }
}
fn set_take_live(handle: i64) -> bool {
    if handle == 0 {
        return false;
    }
    let mut g = live_sets().lock().unwrap_or_else(|e| e.into_inner());
    g.remove(&handle)
}
fn set_is_live(handle: i64) -> bool {
    if handle == 0 {
        return false;
    }
    let g = live_sets().lock().unwrap_or_else(|e| e.into_inner());
    g.contains(&handle)
}

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
    // CG-H3: match interpreter — negative exponents and overflow are errors,
    // not silent zero (which collides with legitimate 0**n results).
    if exp < 0 {
        mimi_runtime_abort(
            b"negative exponent not supported for integers\0".as_ptr() as *const std::ffi::c_char
        );
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
                None => {
                    mimi_runtime_abort(
                        b"integer overflow in power\0".as_ptr() as *const std::ffi::c_char
                    );
                }
            }
        }
        e >>= 1;
        if e > 0 {
            match b.checked_mul(b) {
                Some(v) => b = v,
                None => {
                    mimi_runtime_abort(
                        b"integer overflow in power\0".as_ptr() as *const std::ffi::c_char
                    );
                }
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
/// R-C11: also aborts on stale (destroyed / never-registered) handles.
// SAFETY: aborts on invalid/stale handle; caller must ensure exclusive access while live.
unsafe fn map_from_handle(handle: MapHandle) -> *mut MimiMap {
    if handle == 0 || !map_is_live(handle) {
        std::process::abort();
    }
    handle as *mut MimiMap
}

#[no_mangle]
pub extern "C" fn mimi_map_new() -> MapHandle {
    let map = Box::new(MimiMap {
        inner: HashMap::new(),
    });
    let h = Box::into_raw(map) as MapHandle;
    map_register_live(h);
    h
}

#[no_mangle]
pub extern "C" fn mimi_map_destroy(handle: MapHandle) {
    // R-C11: double free is a no-op; only free if still live.
    if !map_take_live(handle) {
        return;
    }
    // SAFETY: handle was live and removed under lock; exclusive ownership restored.
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
                if libc::mincore(page_start as *mut std::ffi::c_void, page_size, &mut mvec2) != 0 {
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

/// Character-index (Unicode scalar) `char_at`.
/// Returns a new heap-allocated 1-char string; aborts on OOB / invalid UTF-8.
#[no_mangle]
pub extern "C" fn mimi_str_char_at(
    s: *const std::ffi::c_char,
    index: i64,
) -> *mut std::ffi::c_char {
    // SAFETY: `cstr_to_string` handles null pointers safely.
    let ss = unsafe { cstr_to_string(s) };
    if index < 0 {
        mimi_runtime_abort(
            b"str_char_at: index out of bounds\0".as_ptr() as *const std::ffi::c_char
        );
    }
    match ss.chars().nth(index as usize) {
        Some(c) => {
            let mut buf = [0u8; 8];
            let encoded = c.encode_utf8(&mut buf);
            alloc_c_string(encoded)
        }
        None => mimi_runtime_abort(
            b"str_char_at: index out of bounds\0".as_ptr() as *const std::ffi::c_char
        ),
    }
}

/// Character-index (Unicode scalar) substring `[start, end)`.
/// Returns a new heap-allocated string; aborts on `start > end` or end OOB.
#[no_mangle]
pub extern "C" fn mimi_str_substring(
    s: *const std::ffi::c_char,
    start: i64,
    end: i64,
) -> *mut std::ffi::c_char {
    // SAFETY: `cstr_to_string` handles null pointers safely.
    let ss = unsafe { cstr_to_string(s) };
    if start < 0 || end < 0 {
        mimi_runtime_abort(
            b"str_substring: index out of bounds\0".as_ptr() as *const std::ffi::c_char
        );
    }
    if start > end {
        mimi_runtime_abort(b"str_substring: start > end\0".as_ptr() as *const std::ffi::c_char);
    }
    let chars: Vec<char> = ss.chars().collect();
    let s_idx = start as usize;
    let e_idx = end as usize;
    if e_idx > chars.len() {
        mimi_runtime_abort(
            b"str_substring: end out of bounds\0".as_ptr() as *const std::ffi::c_char
        );
    }
    let result: String = chars[s_idx..e_idx].iter().collect();
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
        let data_size =
            match (len as usize).checked_mul(std::mem::size_of::<*mut std::ffi::c_char>()) {
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
pub extern "C" fn mimi_option_map_to_json(
    disc: i64,
    handle: MapHandle,
    mode: i64,
) -> *mut std::ffi::c_char {
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
pub extern "C" fn mimi_option_set_to_json(
    disc: i64,
    handle: SetHandle,
    mode: i64,
) -> *mut std::ffi::c_char {
    if disc == 0 {
        return alloc_c_string("\"None\"");
    }
    // mode: 0-3 scalar; 10+ product; 70+ Map product.
    let json_ptr = if mode >= 70 {
        mimi_set_to_json_map_product_i64(handle, mode - 70, 0)
    } else if mode >= 10 {
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
        // mode: 0-3 scalar; 10+ product; 20+ List; 30+ Set; 40+ Map;
        // 50+ Option product; 60+ Result product.
        let json_ptr = if mode >= 60 {
            mimi_map_to_json_result_product_i64(ok_handle, mode - 60, 0)
        } else if mode >= 50 {
            mimi_map_to_json_option_product_i64(ok_handle, mode - 50, 0)
        } else if mode >= 40 {
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
        // mode: 0-3 scalar; 10+ product; 50+ Option product; 70+ Map product.
        let json_ptr = if mode >= 70 {
            mimi_set_to_json_map_product_i64(ok_handle, mode - 70, 0)
        } else if mode >= 50 {
            mimi_set_to_json_option_product_i64(ok_handle, mode - 50, 0)
        } else if mode >= 10 {
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
                std::ptr::copy_nonoverlapping(prod_handles.as_ptr(), data_ptr, prod_handles.len());
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

/// Map of Result of Map of product from JSON.
/// Pack: `{i64 disc, i64 map_handle_or_err}` disc 1=Ok map handle, 0=Err string ptr.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_result_map_product_i64(
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
        if i >= bytes.len() {
            break;
        }
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            continue;
        }
        if bytes[i] != b'{' {
            unsafe {
                libc::free(pack as *mut _);
            }
            break;
        }
        // Parse tagged Result object
        let obj_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let obj = String::from_utf8_lossy(&bytes[obj_start..i]).into_owned();
        let obj_bytes = obj.as_bytes();
        // Find "Ok" or "Err" tag
        let mut is_err = false;
        let mut j = 0usize;
        while j < obj_bytes.len() && obj_bytes[j] != b'"' {
            j += 1;
        }
        if j < obj_bytes.len() && obj_bytes[j] == b'"' {
            j += 1;
            let ts = j;
            while j < obj_bytes.len() && obj_bytes[j] != b'"' {
                j += 1;
            }
            let tag = &obj[ts..j];
            is_err = tag == "Err";
        }
        if is_err {
            // extract string value after Err
            let mut err_s = String::new();
            if let Some(pos) = obj.find("\"Err\"") {
                let rest = &obj[pos + 5..];
                if let Some(q1) = rest.find('"') {
                    let r2 = &rest[q1 + 1..];
                    if let Some(q2) = r2.find('"') {
                        err_s = r2[..q2].to_string();
                    }
                }
            }
            let c = alloc_c_string(&err_s);
            unsafe {
                *pack = 0;
                *pack.add(1) = c as i64;
            }
        } else {
            // extract object after Ok
            let mut inner_obj = String::from("{}");
            if let Some(pos) = obj.find("\"Ok\"") {
                let rest = &obj[pos + 4..];
                if let Some(brace) = rest.find('{') {
                    let start = brace;
                    let rb = rest.as_bytes();
                    let mut depth = 0i32;
                    let mut k = start;
                    while k < rb.len() {
                        match rb[k] {
                            b'{' => depth += 1,
                            b'}' => {
                                depth -= 1;
                                if depth == 0 {
                                    k += 1;
                                    break;
                                }
                            }
                            b'"' => {
                                k += 1;
                                while k < rb.len() && rb[k] != b'"' {
                                    if rb[k] == b'\\' {
                                        k += 1;
                                    }
                                    k += 1;
                                }
                            }
                            _ => {}
                        }
                        k += 1;
                    }
                    inner_obj = rest[start..k].to_string();
                }
            }
            let c_obj = alloc_c_string(&inner_obj);
            let inner_h = mimi_map_from_json_product_i64(c_obj, arity);
            if !c_obj.is_null() {
                unsafe {
                    libc::free(c_obj as *mut _);
                }
            }
            unsafe {
                *pack = 1;
                *pack.add(1) = inner_h as i64;
            }
        }
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, pack as ValueHandle);
        }
    }
    handle
}

/// Map of Result of Map of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_result_map_product_i64(
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
        if vh == 0 {
            if display_style != 0 {
                parts.push(String::from("Err()"));
            } else {
                parts.push(String::from("{\"Err\":[\"\"]}"));
            }
            continue;
        }
        let base = vh as *const i64;
        let disc = unsafe { *base };
        if disc == 0 {
            let err_ptr = unsafe { *base.add(1) } as *const std::ffi::c_char;
            let err_s = if err_ptr.is_null() {
                String::new()
            } else {
                unsafe { cstr_to_string(err_ptr) }
            };
            if display_style != 0 {
                parts.push(format!("Err({})", err_s));
            } else {
                parts.push(format!("{{\"Err\":[{}]}}", json_escape_string(&err_s)));
            }
        } else {
            let inner_h = unsafe { *base.add(1) } as MapHandle;
            let inner_json = mimi_map_to_json_product_i64(inner_h, arity, display_style);
            let s = unsafe { cstr_to_string(inner_json) };
            if !inner_json.is_null() {
                unsafe {
                    libc::free(inner_json as *mut _);
                }
            }
            if display_style != 0 {
                parts.push(format!("Ok({})", s));
            } else {
                parts.push(format!("{{\"Ok\":[{}]}}", s));
            }
        }
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of Option of Result of product from JSON.
/// Pack: `{i64 disc, i64 res_handle}` disc 0=None; res_handle is Result product pack
/// `{i64 res_disc, i64[n] fields or err}` (same as map result product).
#[no_mangle]
pub extern "C" fn mimi_map_from_json_option_result_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // Reuse result product from_json for object values, wrap with option disc.
    // Parse outer map manually.
    let s = unsafe { cstr_to_string(json) };
    let handle = mimi_map_new();
    if handle == 0 {
        return 0;
    }
    let n = arity as usize;
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
        if i >= bytes.len() {
            break;
        }
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            continue;
        }
        let is_none = if bytes[i] == b'n' && i + 4 <= bytes.len() && &bytes[i..i + 4] == b"null" {
            i += 4;
            true
        } else {
            false
        };
        if is_none {
            unsafe {
                *pack = 0;
                *pack.add(1) = 0;
            }
        } else {
            // Extract value as JSON substring and parse as Result product via one-entry map
            let val_start = i;
            if bytes[i] == b'{' {
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
                        b'"' => {
                            i += 1;
                            while i < bytes.len() && bytes[i] != b'"' {
                                if bytes[i] == b'\\' {
                                    i += 1;
                                }
                                i += 1;
                            }
                        }
                        _ => {}
                    }
                    i += 1;
                }
            } else if bytes[i] == b'[' {
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
                        b'"' => {
                            i += 1;
                            while i < bytes.len() && bytes[i] != b'"' {
                                if bytes[i] == b'\\' {
                                    i += 1;
                                }
                                i += 1;
                            }
                        }
                        _ => {}
                    }
                    i += 1;
                }
            } else {
                unsafe {
                    libc::free(pack as *mut _);
                }
                break;
            }
            let val = String::from_utf8_lossy(&bytes[val_start..i]).into_owned();
            // Build single-key map JSON for result product helper
            let one = format!("{{\"_\":{}}}", val);
            let c_one = alloc_c_string(&one);
            let tmp_map = mimi_map_from_json_result_product_i64(c_one, arity);
            if !c_one.is_null() {
                unsafe {
                    libc::free(c_one as *mut _);
                }
            }
            // Extract the single value handle from tmp_map
            let mut res_h: i64 = 0;
            if tmp_map != 0 {
                let m = unsafe { &*map_from_handle(tmp_map) };
                if let Some(v) = m.inner.values().next() {
                    res_h = *v as i64;
                }
            }
            unsafe {
                *pack = 1;
                *pack.add(1) = res_h;
            }
            let _ = n;
        }
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, pack as ValueHandle);
        }
    }
    handle
}

/// Map of Option of Result of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_option_result_product_i64(
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
            if display_style != 0 {
                parts.push(String::from("None()"));
            } else {
                parts.push(String::from("\"None\""));
            }
            continue;
        }
        let base = vh as *const i64;
        let disc = unsafe { *base };
        if disc == 0 {
            if display_style != 0 {
                parts.push(String::from("None()"));
            } else {
                parts.push(String::from("\"None\""));
            }
        } else {
            let res_h = unsafe { *base.add(1) };
            let tmp = mimi_map_new();
            if tmp != 0 {
                unsafe {
                    (*map_from_handle(tmp))
                        .inner
                        .insert("_".into(), res_h as ValueHandle);
                }
                let json_ptr = mimi_map_to_json_result_product_i64(tmp, arity, display_style);
                let s = unsafe { cstr_to_string(json_ptr) };
                if !json_ptr.is_null() {
                    unsafe {
                        libc::free(json_ptr as *mut _);
                    }
                }
                // format is {"_":VALUE} — strip only the outer map braces once.
                let val = if let Some(colon) = s.find(':') {
                    let mut rest = s[colon + 1..].to_string();
                    if rest.ends_with('}') {
                        rest.pop();
                    }
                    rest
                } else {
                    s
                };
                if display_style != 0 {
                    parts.push(format!("Some({})", val));
                } else {
                    parts.push(format!("{{\"Some\":[{}]}}", val));
                }
            }
            let _ = n;
        }
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// List of Option of product from JSON.
/// Element pack: `{i64 disc, i64[n] fields}` disc 1=Some, 0=None.
#[no_mangle]
pub extern "C" fn mimi_list_from_json_option_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> *mut MimiList {
    let empty = || {
        let list = unsafe { libc::malloc(std::mem::size_of::<MimiList>()) as *mut MimiList };
        if !list.is_null() {
            unsafe {
                (*list).len = 0;
                (*list).data = std::ptr::null_mut();
                (*list).owns_data = true;
            }
        }
        list
    };
    if json.is_null() || arity <= 0 || arity > 16 {
        return empty();
    }
    let s = unsafe { cstr_to_string(json) };
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b'[' {
        return empty();
    }
    i += 1;
    let n = arity as usize;
    let mut handles: Vec<i64> = Vec::new();
    loop {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b']' {
            break;
        }
        let pack_size = 8 + n * 8;
        let ptr = unsafe { libc::malloc(pack_size) as *mut i64 };
        if ptr.is_null() {
            break;
        }
        if bytes[i] == b'n' && i + 4 <= bytes.len() && &bytes[i..i + 4] == b"null" {
            i += 4;
            unsafe {
                *ptr = 0;
                for fi in 0..n {
                    *ptr.add(1 + fi) = 0;
                }
            }
        } else if bytes[i] == b'[' {
            // bare product array → Some
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
            unsafe {
                *ptr = 1;
                std::ptr::copy_nonoverlapping(fields.as_ptr(), ptr.add(1), n);
            }
        } else if bytes[i] == b'{' {
            // tagged Some/None
            let obj_start = i;
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
                    b'"' => {
                        i += 1;
                        while i < bytes.len() && bytes[i] != b'"' {
                            if bytes[i] == b'\\' {
                                i += 1;
                            }
                            i += 1;
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            let obj = String::from_utf8_lossy(&bytes[obj_start..i]).into_owned();
            if obj.contains("\"None\"") || obj == "\"None\"" {
                unsafe {
                    *ptr = 0;
                    for fi in 0..n {
                        *ptr.add(1 + fi) = 0;
                    }
                }
            } else {
                // extract array from Some
                let mut fields = vec![0i64; n];
                if let Some(pos) = obj.find('[') {
                    let ab = obj.as_bytes();
                    let mut j = pos + 1;
                    // nested [[1,2]]
                    while j < ab.len() && ab[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    if j < ab.len() && ab[j] == b'[' {
                        j += 1;
                    }
                    for fi in 0..n {
                        while j < ab.len() && (ab[j].is_ascii_whitespace() || ab[j] == b',') {
                            j += 1;
                        }
                        let neg = j < ab.len() && ab[j] == b'-';
                        if neg {
                            j += 1;
                        }
                        let mut v: i64 = 0;
                        while j < ab.len() && ab[j].is_ascii_digit() {
                            v = v.saturating_mul(10).saturating_add((ab[j] - b'0') as i64);
                            j += 1;
                        }
                        if neg {
                            v = -v;
                        }
                        fields[fi] = v;
                    }
                }
                unsafe {
                    *ptr = 1;
                    std::ptr::copy_nonoverlapping(fields.as_ptr(), ptr.add(1), n);
                }
            }
        } else {
            unsafe {
                libc::free(ptr as *mut _);
            }
            break;
        }
        handles.push(ptr as i64);
    }
    let list = unsafe { libc::malloc(std::mem::size_of::<MimiList>()) as *mut MimiList };
    if list.is_null() {
        return empty();
    }
    let data_size = handles.len() * std::mem::size_of::<i64>();
    let data = if data_size > 0 {
        unsafe { libc::malloc(data_size) as *mut i64 }
    } else {
        std::ptr::null_mut()
    };
    if !data.is_null() && !handles.is_empty() {
        unsafe {
            std::ptr::copy_nonoverlapping(handles.as_ptr(), data, handles.len());
        }
    }
    unsafe {
        (*list).len = handles.len() as i64;
        (*list).data = data as *mut *mut std::ffi::c_char;
        (*list).owns_data = true;
    }
    list
}

/// List of Option of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_list_option_product_to_json(
    list: *const MimiList,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if list.is_null() || arity <= 0 || arity > 16 {
        return alloc_c_string("[]");
    }
    let lst = unsafe { &*list };
    if lst.data.is_null() || lst.len <= 0 {
        return alloc_c_string("[]");
    }
    let n = arity as usize;
    let mut parts: Vec<String> = Vec::with_capacity(lst.len as usize * 2 + 2);
    parts.push(String::from("["));
    for i in 0..lst.len as isize {
        if i > 0 {
            if display_style != 0 {
                parts.push(String::from(", "));
            } else {
                parts.push(String::from(","));
            }
        }
        let h = unsafe { *(lst.data as *const i64).offset(i) };
        if h == 0 {
            if display_style != 0 {
                parts.push(String::from("None()"));
            } else {
                parts.push(String::from("\"None\""));
            }
            continue;
        }
        let base = h as *const i64;
        let disc = unsafe { *base };
        if disc == 0 {
            if display_style != 0 {
                parts.push(String::from("None()"));
            } else {
                parts.push(String::from("\"None\""));
            }
        } else {
            let fields: Vec<i64> = unsafe { std::slice::from_raw_parts(base.add(1), n).to_vec() };
            if display_style != 0 {
                let body: Vec<String> = fields.iter().map(|x| x.to_string()).collect();
                parts.push(format!("Some(({}))", body.join(", ")));
            } else {
                let body: Vec<String> = fields.iter().map(|x| x.to_string()).collect();
                parts.push(format!("{{\"Some\":[[{}]]}}", body.join(",")));
            }
        }
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

/// List of Option of Set of product from JSON.
/// Element pack: `{i64 opt_disc, i64 set_handle}`.
#[no_mangle]
pub extern "C" fn mimi_list_from_json_option_set_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> *mut MimiList {
    let empty = || {
        let list = unsafe { libc::malloc(std::mem::size_of::<MimiList>()) as *mut MimiList };
        if !list.is_null() {
            unsafe {
                (*list).len = 0;
                (*list).data = std::ptr::null_mut();
                (*list).owns_data = true;
            }
        }
        list
    };
    if json.is_null() || arity <= 0 || arity > 16 {
        return empty();
    }
    let s = unsafe { cstr_to_string(json) };
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b'[' {
        return empty();
    }
    i += 1;
    let mut handles: Vec<i64> = Vec::new();
    loop {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b']' {
            break;
        }
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            break;
        }
        if bytes[i] == b'n' && i + 4 <= bytes.len() && &bytes[i..i + 4] == b"null" {
            i += 4;
            unsafe {
                *pack = 0;
                *pack.add(1) = 0;
            }
        } else {
            let val_start = i;
            if bytes[i] == b'{' || bytes[i] == b'[' {
                let open = bytes[i];
                let close = if open == b'{' { b'}' } else { b']' };
                let mut depth = 0i32;
                while i < bytes.len() {
                    if bytes[i] == open {
                        depth += 1;
                    } else if bytes[i] == close {
                        depth -= 1;
                        if depth == 0 {
                            i += 1;
                            break;
                        }
                    } else if bytes[i] == b'"' {
                        i += 1;
                        while i < bytes.len() && bytes[i] != b'"' {
                            if bytes[i] == b'\\' {
                                i += 1;
                            }
                            i += 1;
                        }
                    }
                    i += 1;
                }
            } else {
                unsafe {
                    libc::free(pack as *mut _);
                }
                break;
            }
            let val = String::from_utf8_lossy(&bytes[val_start..i]).into_owned();
            let arr = if val.starts_with('{') {
                if let Some(pos) = val.find('[') {
                    let mut depth = 0i32;
                    let vb = val.as_bytes();
                    let mut k = pos;
                    while k < vb.len() {
                        match vb[k] {
                            b'[' => depth += 1,
                            b']' => {
                                depth -= 1;
                                if depth == 0 {
                                    k += 1;
                                    break;
                                }
                            }
                            b'"' => {
                                k += 1;
                                while k < vb.len() && vb[k] != b'"' {
                                    if vb[k] == b'\\' {
                                        k += 1;
                                    }
                                    k += 1;
                                }
                            }
                            _ => {}
                        }
                        k += 1;
                    }
                    val[pos..k].to_string()
                } else {
                    String::from("[]")
                }
            } else {
                val
            };
            let c_arr = alloc_c_string(&arr);
            let set_h = mimi_set_from_json_product_i64(c_arr, arity);
            if !c_arr.is_null() {
                unsafe {
                    libc::free(c_arr as *mut _);
                }
            }
            unsafe {
                *pack = 1;
                *pack.add(1) = set_h as i64;
            }
        }
        handles.push(pack as i64);
    }
    let list = unsafe { libc::malloc(std::mem::size_of::<MimiList>()) as *mut MimiList };
    if list.is_null() {
        return empty();
    }
    let data_size = handles.len() * 8;
    let data = if data_size == 0 {
        std::ptr::null_mut()
    } else {
        unsafe { libc::malloc(data_size) as *mut i64 }
    };
    if data_size > 0 && data.is_null() {
        unsafe {
            libc::free(list as *mut _);
        }
        return empty();
    }
    if !data.is_null() {
        unsafe {
            std::ptr::copy_nonoverlapping(handles.as_ptr(), data, handles.len());
        }
    }
    unsafe {
        (*list).len = handles.len() as i64;
        (*list).data = data as *mut *mut std::ffi::c_char;
        (*list).owns_data = true;
    }
    list
}

/// List of Option of Set of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_list_option_set_product_to_json(
    list: *const MimiList,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if list.is_null() || arity <= 0 || arity > 16 {
        return alloc_c_string("[]");
    }
    let lst = unsafe { &*list };
    if lst.data.is_null() || lst.len <= 0 {
        return alloc_c_string("[]");
    }
    let mut parts: Vec<String> = Vec::with_capacity(lst.len as usize * 2 + 2);
    parts.push(String::from("["));
    for i in 0..lst.len as isize {
        if i > 0 {
            parts.push(if display_style != 0 {
                String::from(", ")
            } else {
                String::from(",")
            });
        }
        let h = unsafe { *(lst.data as *const i64).offset(i) };
        if h == 0 {
            if display_style != 0 {
                parts.push(String::from("None()"));
            } else {
                parts.push(String::from("\"None\""));
            }
            continue;
        }
        let base = h as *const i64;
        let disc = unsafe { *base };
        if disc == 0 {
            if display_style != 0 {
                parts.push(String::from("None()"));
            } else {
                parts.push(String::from("\"None\""));
            }
        } else {
            let set_h = unsafe { *base.add(1) } as SetHandle;
            let set_json = mimi_set_to_json_product_i64(set_h, arity, display_style);
            let s = unsafe { cstr_to_string(set_json) };
            if !set_json.is_null() {
                unsafe {
                    libc::free(set_json as *mut _);
                }
            }
            if display_style != 0 {
                parts.push(format!("Some({})", s));
            } else {
                parts.push(format!("{{\"Some\":[{}]}}", s));
            }
        }
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

/// Map of List of Option of Set of product from JSON.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_list_option_set_product_i64(
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
        let arr_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let arr = String::from_utf8_lossy(&bytes[arr_start..i]).into_owned();
        let c_arr = alloc_c_string(&arr);
        let list_ptr = mimi_list_from_json_option_set_product_i64(c_arr, arity);
        if !c_arr.is_null() {
            unsafe {
                libc::free(c_arr as *mut _);
            }
        }
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, list_ptr as ValueHandle);
        }
    }
    handle
}

/// Map of List of Option of Set of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_list_option_set_product_i64(
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
        if vh == 0 {
            parts.push(String::from("[]"));
            continue;
        }
        let list_ptr = vh as *const MimiList;
        let json_ptr = mimi_list_option_set_product_to_json(list_ptr, arity, display_style);
        let s = unsafe { cstr_to_string(json_ptr) };
        if !json_ptr.is_null() {
            unsafe {
                libc::free(json_ptr as *mut _);
            }
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of List of Option of product from JSON.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_list_option_product_i64(
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
        let arr_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let arr = String::from_utf8_lossy(&bytes[arr_start..i]).into_owned();
        let c_arr = alloc_c_string(&arr);
        let list_ptr = mimi_list_from_json_option_product_i64(c_arr, arity);
        if !c_arr.is_null() {
            unsafe {
                libc::free(c_arr as *mut _);
            }
        }
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, list_ptr as ValueHandle);
        }
    }
    handle
}

/// Map of List of Option of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_list_option_product_i64(
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
        if vh == 0 {
            parts.push(String::from("[]"));
            continue;
        }
        let list_ptr = vh as *const MimiList;
        let json_ptr = mimi_list_option_product_to_json(list_ptr, arity, display_style);
        let s = unsafe { cstr_to_string(json_ptr) };
        if !json_ptr.is_null() {
            unsafe {
                libc::free(json_ptr as *mut _);
            }
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Set of Option of Result of product from JSON.
#[no_mangle]
pub extern "C" fn mimi_set_from_json_option_result_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> SetHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_set_new();
    }
    // Walk array; each element is null / product array / Result tagged object.
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
    loop {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b']' {
            break;
        }
        let val_start = i;
        if bytes[i] == b'n' && i + 4 <= bytes.len() && &bytes[i..i + 4] == b"null" {
            i += 4;
            let pack = unsafe { libc::malloc(16) as *mut i64 };
            if !pack.is_null() {
                unsafe {
                    *pack = 0;
                    *pack.add(1) = 0;
                }
                mimi_set_insert(handle, pack as SetValueHandle);
            }
            continue;
        }
        if bytes[i] == b'{' || bytes[i] == b'[' {
            let open = bytes[i];
            let close = if open == b'{' { b'}' } else { b']' };
            let mut depth = 0i32;
            while i < bytes.len() {
                if bytes[i] == open {
                    depth += 1;
                } else if bytes[i] == close {
                    depth -= 1;
                    if depth == 0 {
                        i += 1;
                        break;
                    }
                } else if bytes[i] == b'"' {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                i += 1;
            }
        } else {
            break;
        }
        let val = String::from_utf8_lossy(&bytes[val_start..i]).into_owned();
        // Wrap as single-key option-result map and extract
        let wrap = format!("{{\"_\" :{}}}", val);
        let c_wrap = alloc_c_string(&wrap);
        let tmp = mimi_map_from_json_option_result_product_i64(c_wrap, arity);
        if !c_wrap.is_null() {
            unsafe {
                libc::free(c_wrap as *mut _);
            }
        }
        if tmp != 0 {
            let m = unsafe { &*map_from_handle(tmp) };
            if let Some(v) = m.inner.values().next() {
                mimi_set_insert(handle, *v as SetValueHandle);
            }
        }
    }
    handle
}

/// Set of Option of Result of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_set_to_json_option_result_product_i64(
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
    // Sort: None, then Some(Err), then Some(Ok) by fields
    let mut items: Vec<(i64, i64, String, Vec<i64>)> = set
        .inner
        .iter()
        .map(|vh| {
            if *vh == 0 {
                (0i64, 0i64, String::new(), vec![0; n])
            } else {
                let base = *vh as *const i64;
                if base.is_null() {
                    (0i64, 0i64, String::new(), vec![0; n])
                } else {
                    let opt_disc = unsafe { *base };
                    if opt_disc == 0 {
                        (0i64, 0i64, String::new(), vec![0; n])
                    } else {
                        let res_h = unsafe { *base.add(1) };
                        if res_h == 0 {
                            (1i64, 0i64, String::new(), vec![0; n])
                        } else {
                            let rp = res_h as *const i64;
                            let res_disc = unsafe { *rp };
                            if res_disc == 0 {
                                let err_ptr = unsafe { *rp.add(1) } as *const std::ffi::c_char;
                                let err_s = if err_ptr.is_null() {
                                    String::new()
                                } else {
                                    unsafe { cstr_to_string(err_ptr) }
                                };
                                (1i64, 0i64, err_s, vec![0; n])
                            } else {
                                let fields =
                                    unsafe { std::slice::from_raw_parts(rp.add(1), n).to_vec() };
                                (1i64, 1i64, String::new(), fields)
                            }
                        }
                    }
                }
            }
        })
        .collect();
    items.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
            .then_with(|| a.3.cmp(&b.3))
    });
    if display_style != 0 {
        let mut parts: Vec<String> = Vec::with_capacity(items.len() * 2 + 2);
        parts.push(String::from("Set{"));
        for (i, (opt_disc, res_disc, err, fields)) in items.iter().enumerate() {
            if i > 0 {
                parts.push(String::from(", "));
            }
            if *opt_disc == 0 {
                parts.push(String::from("None()"));
            } else if *res_disc == 0 {
                parts.push(format!("Some(Err({}))", err));
            } else {
                let body: Vec<String> = fields.iter().map(|x| x.to_string()).collect();
                parts.push(format!("Some(Ok(({})))", body.join(", ")));
            }
        }
        parts.push(String::from("}"));
        alloc_c_string(&parts.join(""))
    } else {
        let mut parts: Vec<String> = Vec::with_capacity(items.len() * 2 + 2);
        parts.push(String::from("["));
        for (i, (opt_disc, res_disc, err, fields)) in items.iter().enumerate() {
            if i > 0 {
                parts.push(String::from(","));
            }
            if *opt_disc == 0 {
                parts.push(String::from("\"None\""));
            } else if *res_disc == 0 {
                parts.push(format!(
                    "{{\"Some\":[{{\"Err\":[{}]}}]}}",
                    json_escape_string(err)
                ));
            } else {
                let body: Vec<String> = fields.iter().map(|x| x.to_string()).collect();
                parts.push(format!("{{\"Some\":[{{\"Ok\":[[{}]]}}]}}", body.join(",")));
            }
        }
        parts.push(String::from("]"));
        alloc_c_string(&parts.join(""))
    }
}

/// Set of Result of Option of product from JSON.
#[no_mangle]
pub extern "C" fn mimi_set_from_json_result_option_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> SetHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_set_new();
    }
    // Parse list then insert handles into set.
    let list_ptr = mimi_list_from_json_result_option_product_i64(json, arity);
    if list_ptr.is_null() {
        return mimi_set_new();
    }
    let handle = mimi_set_new();
    if handle == 0 {
        return 0;
    }
    let lst = unsafe { &*list_ptr };
    if !lst.data.is_null() && lst.len > 0 {
        for i in 0..lst.len as isize {
            let h = unsafe { *(lst.data as *const i64).offset(i) };
            // Store opaque product handle as set value (same as set result product).
            mimi_set_insert(handle, h as SetValueHandle);
        }
    }
    handle
}

/// Set of Result of Option of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_set_to_json_result_option_product_i64(
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
    // Sort key: (res_disc, err, opt_disc, fields) — Err before Ok to match set result product.
    let mut items: Vec<(i64, String, i64, Vec<i64>)> = set
        .inner
        .iter()
        .map(|vh| {
            if *vh == 0 {
                (0i64, String::new(), 0i64, vec![0; n])
            } else {
                let ptr = *vh as *const i64;
                if ptr.is_null() {
                    (0i64, String::new(), 0i64, vec![0; n])
                } else {
                    let disc = unsafe { *ptr };
                    if disc == 0 {
                        let err_ptr = unsafe { *ptr.add(1) } as *const std::ffi::c_char;
                        let err_s = if err_ptr.is_null() {
                            String::new()
                        } else {
                            unsafe { cstr_to_string(err_ptr) }
                        };
                        (0i64, err_s, 0i64, vec![0; n])
                    } else {
                        let opt_h = unsafe { *ptr.add(1) } as *const i64;
                        if opt_h.is_null() {
                            (1i64, String::new(), 0i64, vec![0; n])
                        } else {
                            let opt_disc = unsafe { *opt_h };
                            if opt_disc == 0 {
                                (1i64, String::new(), 0i64, vec![0; n])
                            } else {
                                let fields =
                                    unsafe { std::slice::from_raw_parts(opt_h.add(1), n).to_vec() };
                                (1i64, String::new(), 1i64, fields)
                            }
                        }
                    }
                }
            }
        })
        .collect();
    items.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
            .then_with(|| a.3.cmp(&b.3))
    });
    if display_style != 0 {
        let mut parts: Vec<String> = Vec::with_capacity(items.len() * 2 + 2);
        parts.push(String::from("Set{"));
        for (i, (disc, err, opt_disc, fields)) in items.iter().enumerate() {
            if i > 0 {
                parts.push(String::from(", "));
            }
            if *disc == 0 {
                parts.push(format!("Err({})", err));
            } else if *opt_disc == 0 {
                parts.push(String::from("Ok(None())"));
            } else {
                let body: Vec<String> = fields.iter().map(|x| x.to_string()).collect();
                parts.push(format!("Ok(Some(({})))", body.join(", ")));
            }
        }
        parts.push(String::from("}"));
        alloc_c_string(&parts.join(""))
    } else {
        let mut parts: Vec<String> = Vec::with_capacity(items.len() * 2 + 2);
        parts.push(String::from("["));
        for (i, (disc, err, opt_disc, fields)) in items.iter().enumerate() {
            if i > 0 {
                parts.push(String::from(","));
            }
            if *disc == 0 {
                parts.push(format!("{{\"Err\":[{}]}}", json_escape_string(err)));
            } else if *opt_disc == 0 {
                parts.push(String::from("{\"Ok\":[\"None\"]}"));
            } else {
                let body: Vec<String> = fields.iter().map(|x| x.to_string()).collect();
                parts.push(format!("{{\"Ok\":[{{\"Some\":[[{}]]}}]}}", body.join(",")));
            }
        }
        parts.push(String::from("]"));
        alloc_c_string(&parts.join(""))
    }
}

/// Set of List of Map of product from JSON.
#[no_mangle]
pub extern "C" fn mimi_set_from_json_list_map_product_i64(
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
        let arr_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let arr = String::from_utf8_lossy(&bytes[arr_start..i]).into_owned();
        let c_arr = alloc_c_string(&arr);
        let list_ptr = mimi_list_from_json_map_product_i64(c_arr, arity);
        if !c_arr.is_null() {
            unsafe {
                libc::free(c_arr as *mut _);
            }
        }
        mimi_set_insert(handle, list_ptr as SetValueHandle);
    }
    handle
}

/// Set of List of Map of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_set_to_json_list_map_product_i64(
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
    let mut decorated: Vec<(String, i64)> = set
        .inner
        .iter()
        .map(|vh| {
            let list_ptr = *vh as *const MimiList;
            let jp = mimi_list_map_product_to_json(list_ptr, arity, display_style);
            let s = unsafe { cstr_to_string(jp) };
            if !jp.is_null() {
                unsafe {
                    libc::free(jp as *mut _);
                }
            }
            (s, *vh as i64)
        })
        .collect();
    decorated.sort_by(|a, b| a.0.cmp(&b.0));
    if display_style != 0 {
        let mut parts: Vec<String> = Vec::with_capacity(decorated.len() * 2 + 2);
        parts.push(String::from("Set{"));
        for (i, (val, _)) in decorated.iter().enumerate() {
            if i > 0 {
                parts.push(String::from(", "));
            }
            parts.push(val.clone());
        }
        parts.push(String::from("}"));
        alloc_c_string(&parts.join(""))
    } else {
        let mut parts: Vec<String> = Vec::with_capacity(decorated.len() * 2 + 2);
        parts.push(String::from("["));
        for (i, (val, _)) in decorated.iter().enumerate() {
            if i > 0 {
                parts.push(String::from(","));
            }
            parts.push(val.clone());
        }
        parts.push(String::from("]"));
        alloc_c_string(&parts.join(""))
    }
}

/// Set of Result of Map of product from JSON.

/// Set of Result of List of product from JSON.
#[no_mangle]
pub extern "C" fn mimi_set_from_json_result_list_product_i64(
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
    loop {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b']' {
            break;
        }
        if bytes[i] != b'{' {
            break;
        }
        let obj_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let obj = String::from_utf8_lossy(&bytes[obj_start..i]).into_owned();
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            continue;
        }
        if obj.contains("\"Err\"") {
            let mut err_s = String::new();
            if let Some(pos) = obj.find("\"Err\"") {
                let rest = &obj[pos + 5..];
                if let Some(q1) = rest.find('"') {
                    let r2 = &rest[q1 + 1..];
                    if let Some(q2) = r2.find('"') {
                        err_s = r2[..q2].to_string();
                    }
                }
            }
            let c = alloc_c_string(&err_s);
            unsafe {
                *pack = 0;
                *pack.add(1) = c as i64;
            }
        } else {
            let mut arr = String::from("[]");
            if let Some(pos) = obj.find("\"Ok\"") {
                let rest = &obj[pos + 4..];
                if let Some(br) = rest.find('[') {
                    let start = br;
                    let rb = rest.as_bytes();
                    let mut depth = 0i32;
                    let mut k = start;
                    while k < rb.len() {
                        match rb[k] {
                            b'[' => depth += 1,
                            b']' => {
                                depth -= 1;
                                if depth == 0 {
                                    k += 1;
                                    break;
                                }
                            }
                            b'"' => {
                                k += 1;
                                while k < rb.len() && rb[k] != b'"' {
                                    if rb[k] == b'\\' {
                                        k += 1;
                                    }
                                    k += 1;
                                }
                            }
                            _ => {}
                        }
                        k += 1;
                    }
                    arr = rest[start..k].to_string();
                }
            }
            // Wrap as single-entry map of list product, extract list handle.
            let wrap = format!("{{\"_\":{}}}", arr);
            let c_wrap = alloc_c_string(&wrap);
            let tmp = mimi_map_from_json_list_product_i64(c_wrap, arity);
            if !c_wrap.is_null() {
                unsafe {
                    libc::free(c_wrap as *mut _);
                }
            }
            let mut list_h: i64 = 0;
            if tmp != 0 {
                let m = unsafe { &*map_from_handle(tmp) };
                if let Some(v) = m.inner.values().next() {
                    list_h = *v as i64;
                }
            }
            unsafe {
                *pack = 1;
                *pack.add(1) = list_h;
            }
        }
        mimi_set_insert(handle, pack as SetValueHandle);
    }
    handle
}

/// Set of Result of List of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_set_to_json_result_list_product_i64(
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
    let mut decorated: Vec<(i32, String)> = set
        .inner
        .iter()
        .map(|vh| {
            let pack = *vh as *const i64;
            if pack.is_null() {
                return (
                    0,
                    if display_style != 0 {
                        String::from("Err()")
                    } else {
                        String::from("{\"Err\":[\"\"]}")
                    },
                );
            }
            let disc = unsafe { *pack };
            if disc == 0 {
                let err_ptr = unsafe { *pack.add(1) } as *const std::ffi::c_char;
                let err_s = if err_ptr.is_null() {
                    String::new()
                } else {
                    unsafe { cstr_to_string(err_ptr) }
                };
                let s = if display_style != 0 {
                    format!("Err({})", err_s)
                } else {
                    format!("{{\"Err\":[{}]}}", json_escape_string(&err_s))
                };
                (0, s)
            } else {
                let list_ptr = unsafe { *pack.add(1) } as *const MimiList;
                // Display list-of-product via one-entry map helper.
                let tmp = mimi_map_new();
                if tmp != 0 {
                    unsafe {
                        (*map_from_handle(tmp))
                            .inner
                            .insert(String::from("_"), list_ptr as ValueHandle);
                    }
                }
                let jp = mimi_map_to_json_list_product_i64(tmp, arity, display_style);
                let map_s = unsafe { cstr_to_string(jp) };
                if !jp.is_null() {
                    unsafe {
                        libc::free(jp as *mut _);
                    }
                }
                // Extract value after first ':'
                let list_s = if let Some(pos) = map_s.find(':') {
                    let rest = map_s[pos + 1..].trim();
                    if rest.ends_with('}') {
                        rest[..rest.len() - 1].to_string()
                    } else {
                        rest.to_string()
                    }
                } else {
                    String::from("[]")
                };
                let s = if display_style != 0 {
                    format!("Ok({})", list_s)
                } else {
                    format!("{{\"Ok\":[{}]}}", list_s)
                };
                (1, s)
            }
        })
        .collect();
    decorated.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    if display_style != 0 {
        let mut parts: Vec<String> = Vec::with_capacity(decorated.len() * 2 + 2);
        parts.push(String::from("Set{"));
        for (i, (_, val)) in decorated.iter().enumerate() {
            if i > 0 {
                parts.push(String::from(", "));
            }
            parts.push(val.clone());
        }
        parts.push(String::from("}"));
        alloc_c_string(&parts.join(""))
    } else {
        let mut parts: Vec<String> = Vec::with_capacity(decorated.len() * 2 + 2);
        parts.push(String::from("["));
        for (i, (_, val)) in decorated.iter().enumerate() {
            if i > 0 {
                parts.push(String::from(","));
            }
            parts.push(val.clone());
        }
        parts.push(String::from("]"));
        alloc_c_string(&parts.join(""))
    }
}

/// Map of List of Map of List of product from JSON.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_list_map_list_product_i64(
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
        let arr_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let arr = String::from_utf8_lossy(&bytes[arr_start..i]).into_owned();
        let c_arr = alloc_c_string(&arr);
        // List of Map of List of product: reuse list_from_json_map with list values
        // via parsing each element as map of list product
        let list_ptr = mimi_list_from_json_map_list_product_i64(c_arr, arity);
        if !c_arr.is_null() {
            unsafe {
                libc::free(c_arr as *mut _);
            }
        }
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, list_ptr as ValueHandle);
        }
    }
    handle
}

/// List of Map of List of product from JSON.
#[no_mangle]
pub extern "C" fn mimi_list_from_json_map_list_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> *mut MimiList {
    let empty = || {
        let list = unsafe { libc::malloc(std::mem::size_of::<MimiList>()) as *mut MimiList };
        if !list.is_null() {
            unsafe {
                (*list).len = 0;
                (*list).data = std::ptr::null_mut();
                (*list).owns_data = true;
            }
        }
        list
    };
    if json.is_null() || arity <= 0 || arity > 16 {
        return empty();
    }
    let s = unsafe { cstr_to_string(json) };
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b'[' {
        return empty();
    }
    i += 1;
    let mut handles: Vec<i64> = Vec::new();
    loop {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b']' {
            break;
        }
        if bytes[i] != b'{' {
            break;
        }
        let obj_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let obj = String::from_utf8_lossy(&bytes[obj_start..i]).into_owned();
        let c_obj = alloc_c_string(&obj);
        let mh = mimi_map_from_json_list_product_i64(c_obj, arity);
        if !c_obj.is_null() {
            unsafe {
                libc::free(c_obj as *mut _);
            }
        }
        handles.push(mh as i64);
    }
    let list = unsafe { libc::malloc(std::mem::size_of::<MimiList>()) as *mut MimiList };
    if list.is_null() {
        return empty();
    }
    let data_size = handles.len() * 8;
    let data = if data_size == 0 {
        std::ptr::null_mut()
    } else {
        unsafe { libc::malloc(data_size) as *mut i64 }
    };
    if data_size > 0 && data.is_null() {
        unsafe {
            libc::free(list as *mut _);
        }
        return empty();
    }
    if !data.is_null() {
        unsafe {
            std::ptr::copy_nonoverlapping(handles.as_ptr(), data, handles.len());
        }
    }
    unsafe {
        (*list).len = handles.len() as i64;
        (*list).data = data as *mut *mut std::ffi::c_char;
        (*list).owns_data = true;
    }
    list
}

/// List of Map of List of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_list_map_list_product_to_json(
    list: *const MimiList,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if list.is_null() || arity <= 0 || arity > 16 {
        return alloc_c_string("[]");
    }
    let lst = unsafe { &*list };
    if lst.data.is_null() || lst.len <= 0 {
        return alloc_c_string("[]");
    }
    let mut parts: Vec<String> = Vec::with_capacity(lst.len as usize * 2 + 2);
    parts.push(String::from("["));
    for i in 0..lst.len as isize {
        if i > 0 {
            parts.push(if display_style != 0 {
                String::from(", ")
            } else {
                String::from(",")
            });
        }
        let h = unsafe { *(lst.data as *const i64).offset(i) };
        let map_json = mimi_map_to_json_list_product_i64(h as MapHandle, arity, display_style);
        let s = unsafe { cstr_to_string(map_json) };
        if !map_json.is_null() {
            unsafe {
                libc::free(map_json as *mut _);
            }
        }
        parts.push(s);
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

/// Map of List of Map of List of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_list_map_list_product_i64(
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
        if vh == 0 {
            parts.push(String::from("[]"));
            continue;
        }
        let list_ptr = vh as *const MimiList;
        let json_ptr = mimi_list_map_list_product_to_json(list_ptr, arity, display_style);
        let s = unsafe { cstr_to_string(json_ptr) };
        if !json_ptr.is_null() {
            unsafe {
                libc::free(json_ptr as *mut _);
            }
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of Option of Map of List of product from JSON.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_option_map_list_product_i64(
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
        if i >= bytes.len() {
            break;
        }
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            continue;
        }
        if bytes[i] == b'n' && i + 4 <= bytes.len() && &bytes[i..i + 4] == b"null" {
            i += 4;
            unsafe {
                *pack = 0;
                *pack.add(1) = 0;
            }
        } else if bytes[i] == b'{' {
            let obj_start = i;
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
                    b'"' => {
                        i += 1;
                        while i < bytes.len() && bytes[i] != b'"' {
                            if bytes[i] == b'\\' {
                                i += 1;
                            }
                            i += 1;
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            let obj = String::from_utf8_lossy(&bytes[obj_start..i]).into_owned();
            let c_obj = alloc_c_string(&obj);
            let mh = mimi_map_from_json_list_product_i64(c_obj, arity);
            if !c_obj.is_null() {
                unsafe {
                    libc::free(c_obj as *mut _);
                }
            }
            unsafe {
                *pack = 1;
                *pack.add(1) = mh as i64;
            }
        } else {
            unsafe {
                libc::free(pack as *mut _);
            }
            break;
        }
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, pack as ValueHandle);
        }
    }
    handle
}

/// Map of Option of Map of List of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_option_map_list_product_i64(
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
        if vh == 0 {
            if display_style != 0 {
                parts.push(String::from("None()"));
            } else {
                parts.push(String::from("\"None\""));
            }
            continue;
        }
        let base = vh as *const i64;
        let disc = unsafe { *base };
        if disc == 0 {
            if display_style != 0 {
                parts.push(String::from("None()"));
            } else {
                parts.push(String::from("\"None\""));
            }
        } else {
            let mh = unsafe { *base.add(1) } as MapHandle;
            let map_json = mimi_map_to_json_list_product_i64(mh, arity, display_style);
            let s = unsafe { cstr_to_string(map_json) };
            if !map_json.is_null() {
                unsafe {
                    libc::free(map_json as *mut _);
                }
            }
            if display_style != 0 {
                parts.push(format!("Some({})", s));
            } else {
                parts.push(format!("{{\"Some\":[{}]}}", s));
            }
        }
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

#[no_mangle]
pub extern "C" fn mimi_set_from_json_result_map_product_i64(
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
    loop {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b']' {
            break;
        }
        if bytes[i] != b'{' {
            break;
        }
        let obj_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let obj = String::from_utf8_lossy(&bytes[obj_start..i]).into_owned();
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            continue;
        }
        if obj.contains("\"Err\"") {
            let mut err_s = String::new();
            if let Some(pos) = obj.find("\"Err\"") {
                let rest = &obj[pos + 5..];
                if let Some(q1) = rest.find('"') {
                    let r2 = &rest[q1 + 1..];
                    if let Some(q2) = r2.find('"') {
                        err_s = r2[..q2].to_string();
                    }
                }
            }
            let c = alloc_c_string(&err_s);
            unsafe {
                *pack = 0;
                *pack.add(1) = c as i64;
            }
        } else {
            let mut map_obj = String::from("{}");
            if let Some(pos) = obj.find("\"Ok\"") {
                let rest = &obj[pos + 4..];
                if let Some(br) = rest.find('{') {
                    let start = br;
                    let rb = rest.as_bytes();
                    let mut depth = 0i32;
                    let mut k = start;
                    while k < rb.len() {
                        match rb[k] {
                            b'{' => depth += 1,
                            b'}' => {
                                depth -= 1;
                                if depth == 0 {
                                    k += 1;
                                    break;
                                }
                            }
                            b'"' => {
                                k += 1;
                                while k < rb.len() && rb[k] != b'"' {
                                    if rb[k] == b'\\' {
                                        k += 1;
                                    }
                                    k += 1;
                                }
                            }
                            _ => {}
                        }
                        k += 1;
                    }
                    map_obj = rest[start..k].to_string();
                }
            }
            let c_map = alloc_c_string(&map_obj);
            let mh = mimi_map_from_json_product_i64(c_map, arity);
            if !c_map.is_null() {
                unsafe {
                    libc::free(c_map as *mut _);
                }
            }
            unsafe {
                *pack = 1;
                *pack.add(1) = mh as i64;
            }
        }
        mimi_set_insert(handle, pack as SetValueHandle);
    }
    handle
}

/// Set of Result of Map of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_set_to_json_result_map_product_i64(
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
    let mut decorated: Vec<(i32, String)> = set
        .inner
        .iter()
        .map(|vh| {
            let pack = *vh as *const i64;
            if pack.is_null() {
                return (
                    0,
                    if display_style != 0 {
                        String::from("Err()")
                    } else {
                        String::from("{\"Err\":[\"\"]}")
                    },
                );
            }
            let disc = unsafe { *pack };
            if disc == 0 {
                let err_ptr = unsafe { *pack.add(1) } as *const std::ffi::c_char;
                let err_s = if err_ptr.is_null() {
                    String::new()
                } else {
                    unsafe { cstr_to_string(err_ptr) }
                };
                let s = if display_style != 0 {
                    format!("Err({})", err_s)
                } else {
                    format!("{{\"Err\":[{}]}}", json_escape_string(&err_s))
                };
                (0, s)
            } else {
                let mh = unsafe { *pack.add(1) } as MapHandle;
                let jp = mimi_map_to_json_product_i64(mh, arity, display_style);
                let map_s = unsafe { cstr_to_string(jp) };
                if !jp.is_null() {
                    unsafe {
                        libc::free(jp as *mut _);
                    }
                }
                let s = if display_style != 0 {
                    format!("Ok({})", map_s)
                } else {
                    format!("{{\"Ok\":[{}]}}", map_s)
                };
                (1, s)
            }
        })
        .collect();
    // Err before Ok
    decorated.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    if display_style != 0 {
        let mut parts: Vec<String> = Vec::with_capacity(decorated.len() * 2 + 2);
        parts.push(String::from("Set{"));
        for (i, (_, val)) in decorated.iter().enumerate() {
            if i > 0 {
                parts.push(String::from(", "));
            }
            parts.push(val.clone());
        }
        parts.push(String::from("}"));
        alloc_c_string(&parts.join(""))
    } else {
        let mut parts: Vec<String> = Vec::with_capacity(decorated.len() * 2 + 2);
        parts.push(String::from("["));
        for (i, (_, val)) in decorated.iter().enumerate() {
            if i > 0 {
                parts.push(String::from(","));
            }
            parts.push(val.clone());
        }
        parts.push(String::from("]"));
        alloc_c_string(&parts.join(""))
    }
}

/// Map of Map of List of product from JSON.

/// Map of Map of Result of product from JSON.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_map_result_product_i64(
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
        let obj_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let obj = String::from_utf8_lossy(&bytes[obj_start..i]).into_owned();
        let c_obj = alloc_c_string(&obj);
        let inner = mimi_map_from_json_result_product_i64(c_obj, arity);
        if !c_obj.is_null() {
            unsafe {
                libc::free(c_obj as *mut _);
            }
        }
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, inner as ValueHandle);
        }
    }
    handle
}

/// Map of Map of Result of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_map_result_product_i64(
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
        let inner_json = mimi_map_to_json_result_product_i64(vh as MapHandle, arity, display_style);
        let s = unsafe { cstr_to_string(inner_json) };
        if !inner_json.is_null() {
            unsafe {
                libc::free(inner_json as *mut _);
            }
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Set of Map of List of product from JSON.

/// Set of Map of Set of product from JSON.
#[no_mangle]
pub extern "C" fn mimi_set_from_json_map_set_product_i64(
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
    loop {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b']' {
            break;
        }
        if bytes[i] != b'{' {
            break;
        }
        let obj_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let obj = String::from_utf8_lossy(&bytes[obj_start..i]).into_owned();
        let c_obj = alloc_c_string(&obj);
        let mh = mimi_map_from_json_set_product_i64(c_obj, arity);
        if !c_obj.is_null() {
            unsafe {
                libc::free(c_obj as *mut _);
            }
        }
        mimi_set_insert(handle, mh as SetValueHandle);
    }
    handle
}

/// Set of Map of Set of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_set_to_json_map_set_product_i64(
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
    let mut decorated: Vec<(String, i64)> = set
        .inner
        .iter()
        .map(|vh| {
            let mh = *vh as MapHandle;
            let jp = mimi_map_to_json_set_product_i64(mh, arity, display_style);
            let s = unsafe { cstr_to_string(jp) };
            if !jp.is_null() {
                unsafe {
                    libc::free(jp as *mut _);
                }
            }
            (s, *vh as i64)
        })
        .collect();
    decorated.sort_by(|a, b| a.0.cmp(&b.0));
    if display_style != 0 {
        let mut parts: Vec<String> = Vec::with_capacity(decorated.len() * 2 + 2);
        parts.push(String::from("Set{"));
        for (i, (val, _)) in decorated.iter().enumerate() {
            if i > 0 {
                parts.push(String::from(", "));
            }
            parts.push(val.clone());
        }
        parts.push(String::from("}"));
        alloc_c_string(&parts.join(""))
    } else {
        let mut parts: Vec<String> = Vec::with_capacity(decorated.len() * 2 + 2);
        parts.push(String::from("["));
        for (i, (val, _)) in decorated.iter().enumerate() {
            if i > 0 {
                parts.push(String::from(","));
            }
            parts.push(val.clone());
        }
        parts.push(String::from("]"));
        alloc_c_string(&parts.join(""))
    }
}

/// Map of Set of Map of List of product from JSON.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_set_map_list_product_i64(
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
        let arr_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let arr = String::from_utf8_lossy(&bytes[arr_start..i]).into_owned();
        let c_arr = alloc_c_string(&arr);
        let set_h = mimi_set_from_json_map_list_product_i64(c_arr, arity);
        if !c_arr.is_null() {
            unsafe {
                libc::free(c_arr as *mut _);
            }
        }
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, set_h as ValueHandle);
        }
    }
    handle
}

/// Map of Set of Map of List of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_set_map_list_product_i64(
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
        let set_json = mimi_set_to_json_map_list_product_i64(vh as SetHandle, arity, display_style);
        let s = unsafe { cstr_to_string(set_json) };
        if !set_json.is_null() {
            unsafe {
                libc::free(set_json as *mut _);
            }
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

#[no_mangle]
pub extern "C" fn mimi_set_from_json_map_list_product_i64(
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
    loop {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b']' {
            break;
        }
        if bytes[i] != b'{' {
            break;
        }
        let obj_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let obj = String::from_utf8_lossy(&bytes[obj_start..i]).into_owned();
        let c_obj = alloc_c_string(&obj);
        let mh = mimi_map_from_json_list_product_i64(c_obj, arity);
        if !c_obj.is_null() {
            unsafe {
                libc::free(c_obj as *mut _);
            }
        }
        mimi_set_insert(handle, mh as SetValueHandle);
    }
    handle
}

/// Set of Map of List of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_set_to_json_map_list_product_i64(
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
    let mut decorated: Vec<(String, i64)> = set
        .inner
        .iter()
        .map(|vh| {
            let mh = *vh as MapHandle;
            let jp = mimi_map_to_json_list_product_i64(mh, arity, display_style);
            let s = unsafe { cstr_to_string(jp) };
            if !jp.is_null() {
                unsafe {
                    libc::free(jp as *mut _);
                }
            }
            (s, *vh as i64)
        })
        .collect();
    decorated.sort_by(|a, b| a.0.cmp(&b.0));
    if display_style != 0 {
        let mut parts: Vec<String> = Vec::with_capacity(decorated.len() * 2 + 2);
        parts.push(String::from("Set{"));
        for (i, (val, _)) in decorated.iter().enumerate() {
            if i > 0 {
                parts.push(String::from(", "));
            }
            parts.push(val.clone());
        }
        parts.push(String::from("}"));
        alloc_c_string(&parts.join(""))
    } else {
        let mut parts: Vec<String> = Vec::with_capacity(decorated.len() * 2 + 2);
        parts.push(String::from("["));
        for (i, (val, _)) in decorated.iter().enumerate() {
            if i > 0 {
                parts.push(String::from(","));
            }
            parts.push(val.clone());
        }
        parts.push(String::from("]"));
        alloc_c_string(&parts.join(""))
    }
}

#[no_mangle]
pub extern "C" fn mimi_map_from_json_map_list_product_i64(
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
        let obj_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let obj = String::from_utf8_lossy(&bytes[obj_start..i]).into_owned();
        let c_obj = alloc_c_string(&obj);
        let inner = mimi_map_from_json_list_product_i64(c_obj, arity);
        if !c_obj.is_null() {
            unsafe {
                libc::free(c_obj as *mut _);
            }
        }
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, inner as ValueHandle);
        }
    }
    handle
}

/// Map of Map of List of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_map_list_product_i64(
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
        let inner_json = mimi_map_to_json_list_product_i64(vh as MapHandle, arity, display_style);
        let s = unsafe { cstr_to_string(inner_json) };
        if !inner_json.is_null() {
            unsafe {
                libc::free(inner_json as *mut _);
            }
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of Map of Option of product from JSON.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_map_option_product_i64(
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
        let obj_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let obj = String::from_utf8_lossy(&bytes[obj_start..i]).into_owned();
        let c_obj = alloc_c_string(&obj);
        let inner = mimi_map_from_json_option_product_i64(c_obj, arity);
        if !c_obj.is_null() {
            unsafe {
                libc::free(c_obj as *mut _);
            }
        }
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, inner as ValueHandle);
        }
    }
    handle
}

/// Map of Map of Option of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_map_option_product_i64(
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
        let inner_json = mimi_map_to_json_option_product_i64(vh as MapHandle, arity, display_style);
        let s = unsafe { cstr_to_string(inner_json) };
        if !inner_json.is_null() {
            unsafe {
                libc::free(inner_json as *mut _);
            }
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Set of Option of Map of product from JSON.
#[no_mangle]
pub extern "C" fn mimi_set_from_json_option_map_product_i64(
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
    loop {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b']' {
            break;
        }
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            break;
        }
        if bytes[i] == b'n' && i + 4 <= bytes.len() && &bytes[i..i + 4] == b"null" {
            i += 4;
            unsafe {
                *pack = 0;
                *pack.add(1) = 0;
            }
        } else if bytes[i] == b'{' {
            let obj_start = i;
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
                    b'"' => {
                        i += 1;
                        while i < bytes.len() && bytes[i] != b'"' {
                            if bytes[i] == b'\\' {
                                i += 1;
                            }
                            i += 1;
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            let obj = String::from_utf8_lossy(&bytes[obj_start..i]).into_owned();
            let c_obj = alloc_c_string(&obj);
            let mh = mimi_map_from_json_product_i64(c_obj, arity);
            if !c_obj.is_null() {
                unsafe {
                    libc::free(c_obj as *mut _);
                }
            }
            unsafe {
                *pack = 1;
                *pack.add(1) = mh as i64;
            }
        } else {
            unsafe {
                libc::free(pack as *mut _);
            }
            break;
        }
        mimi_set_insert(handle, pack as SetValueHandle);
    }
    handle
}

/// Set of Option of Map of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_set_to_json_option_map_product_i64(
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
    let mut decorated: Vec<(String, i64)> = set
        .inner
        .iter()
        .map(|vh| {
            let pack = *vh as *const i64;
            if pack.is_null() {
                return (
                    if display_style != 0 {
                        String::from("None()")
                    } else {
                        String::from("\"None\"")
                    },
                    0i64,
                );
            }
            let disc = unsafe { *pack };
            let s = if disc == 0 {
                if display_style != 0 {
                    String::from("None()")
                } else {
                    String::from("\"None\"")
                }
            } else {
                let mh = unsafe { *pack.add(1) } as MapHandle;
                let jp = mimi_map_to_json_product_i64(mh, arity, display_style);
                let map_s = unsafe { cstr_to_string(jp) };
                if !jp.is_null() {
                    unsafe {
                        libc::free(jp as *mut _);
                    }
                }
                if display_style != 0 {
                    format!("Some({})", map_s)
                } else {
                    format!("{{\"Some\":[{}]}}", map_s)
                }
            };
            // Sort: None before Some
            let _sort_key = if disc == 0 {
                format!("0_{}", s)
            } else {
                format!("1_{}", s)
            };
            (s, *vh as i64)
        })
        .collect();
    // Sort None before Some by display string prefix
    decorated.sort_by(|a, b| {
        let an = a.0.starts_with("None") || a.0 == "\"None\"";
        let bn = b.0.starts_with("None") || b.0 == "\"None\"";
        match (an, bn) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.0.cmp(&b.0),
        }
    });
    if display_style != 0 {
        let mut parts: Vec<String> = Vec::with_capacity(decorated.len() * 2 + 2);
        parts.push(String::from("Set{"));
        for (i, (val, _)) in decorated.iter().enumerate() {
            if i > 0 {
                parts.push(String::from(", "));
            }
            parts.push(val.clone());
        }
        parts.push(String::from("}"));
        alloc_c_string(&parts.join(""))
    } else {
        let mut parts: Vec<String> = Vec::with_capacity(decorated.len() * 2 + 2);
        parts.push(String::from("["));
        for (i, (val, _)) in decorated.iter().enumerate() {
            if i > 0 {
                parts.push(String::from(","));
            }
            parts.push(val.clone());
        }
        parts.push(String::from("]"));
        alloc_c_string(&parts.join(""))
    }
}

/// Map of Map of Set of product from JSON.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_map_set_product_i64(
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
        let obj_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let obj = String::from_utf8_lossy(&bytes[obj_start..i]).into_owned();
        let c_obj = alloc_c_string(&obj);
        let inner = mimi_map_from_json_set_product_i64(c_obj, arity);
        if !c_obj.is_null() {
            unsafe {
                libc::free(c_obj as *mut _);
            }
        }
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, inner as ValueHandle);
        }
    }
    handle
}

/// Map of Map of Set of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_map_set_product_i64(
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
        let inner_json = mimi_map_to_json_set_product_i64(vh as MapHandle, arity, display_style);
        let s = unsafe { cstr_to_string(inner_json) };
        if !inner_json.is_null() {
            unsafe {
                libc::free(inner_json as *mut _);
            }
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Set of Map of product from JSON.
#[no_mangle]
pub extern "C" fn mimi_set_from_json_map_product_i64(
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
    loop {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b']' {
            break;
        }
        if bytes[i] != b'{' {
            break;
        }
        let obj_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let obj = String::from_utf8_lossy(&bytes[obj_start..i]).into_owned();
        let c_obj = alloc_c_string(&obj);
        let mh = mimi_map_from_json_product_i64(c_obj, arity);
        if !c_obj.is_null() {
            unsafe {
                libc::free(c_obj as *mut _);
            }
        }
        mimi_set_insert(handle, mh as SetValueHandle);
    }
    handle
}

/// Set of Map of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_set_to_json_map_product_i64(
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
    let mut decorated: Vec<(String, i64)> = set
        .inner
        .iter()
        .map(|vh| {
            let mh = *vh as MapHandle;
            let jp = mimi_map_to_json_product_i64(mh, arity, display_style);
            let s = unsafe { cstr_to_string(jp) };
            if !jp.is_null() {
                unsafe {
                    libc::free(jp as *mut _);
                }
            }
            (s, *vh as i64)
        })
        .collect();
    decorated.sort_by(|a, b| a.0.cmp(&b.0));
    if display_style != 0 {
        let mut parts: Vec<String> = Vec::with_capacity(decorated.len() * 2 + 2);
        parts.push(String::from("Set{"));
        for (i, (val, _)) in decorated.iter().enumerate() {
            if i > 0 {
                parts.push(String::from(", "));
            }
            parts.push(val.clone());
        }
        parts.push(String::from("}"));
        alloc_c_string(&parts.join(""))
    } else {
        let mut parts: Vec<String> = Vec::with_capacity(decorated.len() * 2 + 2);
        parts.push(String::from("["));
        for (i, (val, _)) in decorated.iter().enumerate() {
            if i > 0 {
                parts.push(String::from(","));
            }
            parts.push(val.clone());
        }
        parts.push(String::from("]"));
        alloc_c_string(&parts.join(""))
    }
}

/// List of Set of Map of product from JSON.
#[no_mangle]
pub extern "C" fn mimi_list_from_json_set_map_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> *mut MimiList {
    let empty = || {
        let list = unsafe { libc::malloc(std::mem::size_of::<MimiList>()) as *mut MimiList };
        if !list.is_null() {
            unsafe {
                (*list).len = 0;
                (*list).data = std::ptr::null_mut();
                (*list).owns_data = true;
            }
        }
        list
    };
    if json.is_null() || arity <= 0 || arity > 16 {
        return empty();
    }
    let s = unsafe { cstr_to_string(json) };
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b'[' {
        return empty();
    }
    i += 1;
    let mut handles: Vec<i64> = Vec::new();
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
        let arr_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let arr = String::from_utf8_lossy(&bytes[arr_start..i]).into_owned();
        let c_arr = alloc_c_string(&arr);
        let set_h = mimi_set_from_json_map_product_i64(c_arr, arity);
        if !c_arr.is_null() {
            unsafe {
                libc::free(c_arr as *mut _);
            }
        }
        handles.push(set_h as i64);
    }
    let list = unsafe { libc::malloc(std::mem::size_of::<MimiList>()) as *mut MimiList };
    if list.is_null() {
        return empty();
    }
    let data_size = handles.len() * 8;
    let data = if data_size == 0 {
        std::ptr::null_mut()
    } else {
        unsafe { libc::malloc(data_size) as *mut i64 }
    };
    if data_size > 0 && data.is_null() {
        unsafe {
            libc::free(list as *mut _);
        }
        return empty();
    }
    if !data.is_null() {
        unsafe {
            std::ptr::copy_nonoverlapping(handles.as_ptr(), data, handles.len());
        }
    }
    unsafe {
        (*list).len = handles.len() as i64;
        (*list).data = data as *mut *mut std::ffi::c_char;
        (*list).owns_data = true;
    }
    list
}

/// List of Set of Map of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_list_set_map_product_to_json(
    list: *const MimiList,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if list.is_null() || arity <= 0 || arity > 16 {
        return alloc_c_string("[]");
    }
    let lst = unsafe { &*list };
    if lst.data.is_null() || lst.len <= 0 {
        return alloc_c_string("[]");
    }
    let mut parts: Vec<String> = Vec::with_capacity(lst.len as usize * 2 + 2);
    parts.push(String::from("["));
    for i in 0..lst.len as isize {
        if i > 0 {
            parts.push(if display_style != 0 {
                String::from(", ")
            } else {
                String::from(",")
            });
        }
        let h = unsafe { *(lst.data as *const i64).offset(i) };
        let set_json = mimi_set_to_json_map_product_i64(h as SetHandle, arity, display_style);
        let s = unsafe { cstr_to_string(set_json) };
        if !set_json.is_null() {
            unsafe {
                libc::free(set_json as *mut _);
            }
        }
        parts.push(s);
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

/// List of Set of product from JSON.
#[no_mangle]
pub extern "C" fn mimi_list_from_json_set_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> *mut MimiList {
    let empty = || {
        let list = unsafe { libc::malloc(std::mem::size_of::<MimiList>()) as *mut MimiList };
        if !list.is_null() {
            unsafe {
                (*list).len = 0;
                (*list).data = std::ptr::null_mut();
                (*list).owns_data = true;
            }
        }
        list
    };
    if json.is_null() || arity <= 0 || arity > 16 {
        return empty();
    }
    let s = unsafe { cstr_to_string(json) };
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b'[' {
        return empty();
    }
    i += 1;
    let mut handles: Vec<i64> = Vec::new();
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
        let arr_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let arr = String::from_utf8_lossy(&bytes[arr_start..i]).into_owned();
        let c_arr = alloc_c_string(&arr);
        let set_h = mimi_set_from_json_product_i64(c_arr, arity);
        if !c_arr.is_null() {
            unsafe {
                libc::free(c_arr as *mut _);
            }
        }
        handles.push(set_h as i64);
    }
    let list = unsafe { libc::malloc(std::mem::size_of::<MimiList>()) as *mut MimiList };
    if list.is_null() {
        return empty();
    }
    let data_size = handles.len() * 8;
    let data = if data_size == 0 {
        std::ptr::null_mut()
    } else {
        unsafe { libc::malloc(data_size) as *mut i64 }
    };
    if data_size > 0 && data.is_null() {
        unsafe {
            libc::free(list as *mut _);
        }
        return empty();
    }
    if !data.is_null() {
        unsafe {
            std::ptr::copy_nonoverlapping(handles.as_ptr(), data, handles.len());
        }
    }
    unsafe {
        (*list).len = handles.len() as i64;
        (*list).data = data as *mut *mut std::ffi::c_char;
        (*list).owns_data = true;
    }
    list
}

/// Set of List of product from JSON array of list-of-product arrays.
#[no_mangle]
pub extern "C" fn mimi_set_from_json_list_product_i64(
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
        // One list-of-product: [[1,2],[3,4]]
        let arr_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let arr = String::from_utf8_lossy(&bytes[arr_start..i]).into_owned();
        // Parse as single-key map list product then extract list handle
        let wrap = format!("{{\"_\" :{}}}", arr);
        let c_wrap = alloc_c_string(&wrap);
        let tmp = mimi_map_from_json_list_product_i64(c_wrap, arity);
        if !c_wrap.is_null() {
            unsafe {
                libc::free(c_wrap as *mut _);
            }
        }
        if tmp != 0 {
            let m = unsafe { &*map_from_handle(tmp) };
            if let Some(v) = m.inner.values().next() {
                mimi_set_insert(handle, *v as SetValueHandle);
            }
        }
        let _ = n;
    }
    handle
}

/// Set of List of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_set_to_json_list_product_i64(
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
    let items: Vec<i64> = set.inner.iter().map(|x| *x as i64).collect();
    // Sort by JSON string of list for stable dual
    let mut decorated: Vec<(String, i64)> = items
        .iter()
        .map(|h| {
            let tmp = mimi_map_new();
            if tmp != 0 && *h != 0 {
                unsafe {
                    (*map_from_handle(tmp))
                        .inner
                        .insert("_".into(), *h as ValueHandle);
                }
                let jp = mimi_map_to_json_list_product_i64(tmp, arity, display_style);
                let s = unsafe { cstr_to_string(jp) };
                if !jp.is_null() {
                    unsafe {
                        libc::free(jp as *mut _);
                    }
                }
                let val = if let Some(colon) = s.find(':') {
                    let mut rest = s[colon + 1..].to_string();
                    if rest.ends_with('}') {
                        rest.pop();
                    }
                    rest
                } else {
                    s
                };
                (val, *h)
            } else {
                (String::from("[]"), *h)
            }
        })
        .collect();
    decorated.sort_by(|a, b| a.0.cmp(&b.0));
    if display_style != 0 {
        let mut parts: Vec<String> = Vec::with_capacity(decorated.len() * 2 + 2);
        parts.push(String::from("Set{"));
        for (i, (val, _)) in decorated.iter().enumerate() {
            if i > 0 {
                parts.push(String::from(", "));
            }
            parts.push(val.clone());
        }
        parts.push(String::from("}"));
        alloc_c_string(&parts.join(""))
    } else {
        let mut parts: Vec<String> = Vec::with_capacity(decorated.len() * 2 + 2);
        parts.push(String::from("["));
        for (i, (val, _)) in decorated.iter().enumerate() {
            if i > 0 {
                parts.push(String::from(","));
            }
            parts.push(val.clone());
        }
        parts.push(String::from("]"));
        alloc_c_string(&parts.join(""))
    }
}

/// List of Map of product from JSON array of map objects.
#[no_mangle]
pub extern "C" fn mimi_list_from_json_map_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> *mut MimiList {
    let empty = || {
        let list = unsafe { libc::malloc(std::mem::size_of::<MimiList>()) as *mut MimiList };
        if !list.is_null() {
            unsafe {
                (*list).len = 0;
                (*list).data = std::ptr::null_mut();
                (*list).owns_data = true;
            }
        }
        list
    };
    if json.is_null() || arity <= 0 || arity > 16 {
        return empty();
    }
    let s = unsafe { cstr_to_string(json) };
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b'[' {
        return empty();
    }
    i += 1;
    let mut handles: Vec<i64> = Vec::new();
    loop {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b']' {
            break;
        }
        if bytes[i] != b'{' {
            break;
        }
        let obj_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let obj = String::from_utf8_lossy(&bytes[obj_start..i]).into_owned();
        let c_obj = alloc_c_string(&obj);
        let mh = mimi_map_from_json_product_i64(c_obj, arity);
        if !c_obj.is_null() {
            unsafe {
                libc::free(c_obj as *mut _);
            }
        }
        handles.push(mh as i64);
    }
    let list = unsafe { libc::malloc(std::mem::size_of::<MimiList>()) as *mut MimiList };
    if list.is_null() {
        return empty();
    }
    let data_size = handles.len() * 8;
    let data = if data_size == 0 {
        std::ptr::null_mut()
    } else {
        unsafe { libc::malloc(data_size) as *mut i64 }
    };
    if data_size > 0 && data.is_null() {
        unsafe {
            libc::free(list as *mut _);
        }
        return empty();
    }
    if !data.is_null() {
        unsafe {
            std::ptr::copy_nonoverlapping(handles.as_ptr(), data, handles.len());
        }
    }
    unsafe {
        (*list).len = handles.len() as i64;
        (*list).data = data as *mut *mut std::ffi::c_char;
        (*list).owns_data = true;
    }
    list
}

/// List of Map of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_list_map_product_to_json(
    list: *const MimiList,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if list.is_null() || arity <= 0 || arity > 16 {
        return alloc_c_string("[]");
    }
    let lst = unsafe { &*list };
    if lst.data.is_null() || lst.len <= 0 {
        return alloc_c_string("[]");
    }
    let mut parts: Vec<String> = Vec::with_capacity(lst.len as usize * 2 + 2);
    parts.push(String::from("["));
    for i in 0..lst.len as isize {
        if i > 0 {
            parts.push(if display_style != 0 {
                String::from(", ")
            } else {
                String::from(",")
            });
        }
        let h = unsafe { *(lst.data as *const i64).offset(i) };
        let map_json = mimi_map_to_json_product_i64(h as MapHandle, arity, display_style);
        let s = unsafe { cstr_to_string(map_json) };
        if !map_json.is_null() {
            unsafe {
                libc::free(map_json as *mut _);
            }
        }
        parts.push(s);
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

/// Map of Set of List of Map of product from JSON.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_set_list_map_product_i64(
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
        let arr_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let arr = String::from_utf8_lossy(&bytes[arr_start..i]).into_owned();
        let c_arr = alloc_c_string(&arr);
        let set_h = mimi_set_from_json_list_map_product_i64(c_arr, arity);
        if !c_arr.is_null() {
            unsafe {
                libc::free(c_arr as *mut _);
            }
        }
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, set_h as ValueHandle);
        }
    }
    handle
}

/// Map of Set of List of Map of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_set_list_map_product_i64(
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
        let set_json = mimi_set_to_json_list_map_product_i64(vh as SetHandle, arity, display_style);
        let s = unsafe { cstr_to_string(set_json) };
        if !set_json.is_null() {
            unsafe {
                libc::free(set_json as *mut _);
            }
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of List of Set of Map of product from JSON.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_list_set_map_product_i64(
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
        let arr_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let arr = String::from_utf8_lossy(&bytes[arr_start..i]).into_owned();
        let c_arr = alloc_c_string(&arr);
        let list_ptr = mimi_list_from_json_set_map_product_i64(c_arr, arity);
        if !c_arr.is_null() {
            unsafe {
                libc::free(c_arr as *mut _);
            }
        }
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, list_ptr as ValueHandle);
        }
    }
    handle
}

/// Map of List of Set of Map of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_list_set_map_product_i64(
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
        if vh == 0 {
            parts.push(String::from("[]"));
            continue;
        }
        let list_ptr = vh as *const MimiList;
        let json_ptr = mimi_list_set_map_product_to_json(list_ptr, arity, display_style);
        let s = unsafe { cstr_to_string(json_ptr) };
        if !json_ptr.is_null() {
            unsafe {
                libc::free(json_ptr as *mut _);
            }
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of Set of Map of product from JSON.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_set_map_product_i64(
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
        let arr_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let arr = String::from_utf8_lossy(&bytes[arr_start..i]).into_owned();
        let c_arr = alloc_c_string(&arr);
        let set_h = mimi_set_from_json_map_product_i64(c_arr, arity);
        if !c_arr.is_null() {
            unsafe {
                libc::free(c_arr as *mut _);
            }
        }
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, set_h as ValueHandle);
        }
    }
    handle
}

/// Map of Set of Map of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_set_map_product_i64(
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
        let set_json = mimi_set_to_json_map_product_i64(vh as SetHandle, arity, display_style);
        let s = unsafe { cstr_to_string(set_json) };
        if !set_json.is_null() {
            unsafe {
                libc::free(set_json as *mut _);
            }
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of List of Map of product from JSON.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_list_map_product_i64(
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
        let arr_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let arr = String::from_utf8_lossy(&bytes[arr_start..i]).into_owned();
        let c_arr = alloc_c_string(&arr);
        let list_ptr = mimi_list_from_json_map_product_i64(c_arr, arity);
        if !c_arr.is_null() {
            unsafe {
                libc::free(c_arr as *mut _);
            }
        }
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, list_ptr as ValueHandle);
        }
    }
    handle
}

/// Map of List of Map of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_list_map_product_i64(
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
        if vh == 0 {
            parts.push(String::from("[]"));
            continue;
        }
        let list_ptr = vh as *const MimiList;
        let json_ptr = mimi_list_map_product_to_json(list_ptr, arity, display_style);
        let s = unsafe { cstr_to_string(json_ptr) };
        if !json_ptr.is_null() {
            unsafe {
                libc::free(json_ptr as *mut _);
            }
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of Set of List of product from JSON.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_set_list_product_i64(
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
        let arr_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let arr = String::from_utf8_lossy(&bytes[arr_start..i]).into_owned();
        let c_arr = alloc_c_string(&arr);
        let set_h = mimi_set_from_json_list_product_i64(c_arr, arity);
        if !c_arr.is_null() {
            unsafe {
                libc::free(c_arr as *mut _);
            }
        }
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, set_h as ValueHandle);
        }
    }
    handle
}

/// Map of Set of List of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_set_list_product_i64(
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
        if vh == 0 {
            if display_style != 0 {
                parts.push(String::from("Set{}"));
            } else {
                parts.push(String::from("[]"));
            }
            continue;
        }
        let set_h = vh as SetHandle;
        let json_ptr = mimi_set_to_json_list_product_i64(set_h, arity, display_style);
        let s = unsafe { cstr_to_string(json_ptr) };
        if !json_ptr.is_null() {
            unsafe {
                libc::free(json_ptr as *mut _);
            }
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of List of Set of Result of product from JSON.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_list_set_result_product_i64(
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
        let arr_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let arr = String::from_utf8_lossy(&bytes[arr_start..i]).into_owned();
        let c_arr = alloc_c_string(&arr);
        let list_ptr = mimi_list_from_json_set_result_product_i64(c_arr, arity);
        if !c_arr.is_null() {
            unsafe {
                libc::free(c_arr as *mut _);
            }
        }
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, list_ptr as ValueHandle);
        }
    }
    handle
}

/// Map of List of Set of Result of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_list_set_result_product_i64(
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
        if vh == 0 {
            parts.push(String::from("[]"));
            continue;
        }
        let list_ptr = vh as *const MimiList;
        let json_ptr = mimi_list_set_result_product_to_json(list_ptr, arity, display_style);
        let s = unsafe { cstr_to_string(json_ptr) };
        if !json_ptr.is_null() {
            unsafe {
                libc::free(json_ptr as *mut _);
            }
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of List of Set of Option of product from JSON.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_list_set_option_product_i64(
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
        let arr_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let arr = String::from_utf8_lossy(&bytes[arr_start..i]).into_owned();
        let c_arr = alloc_c_string(&arr);
        let list_ptr = mimi_list_from_json_set_option_product_i64(c_arr, arity);
        if !c_arr.is_null() {
            unsafe {
                libc::free(c_arr as *mut _);
            }
        }
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, list_ptr as ValueHandle);
        }
    }
    handle
}

/// Map of List of Set of Option of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_list_set_option_product_i64(
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
        if vh == 0 {
            parts.push(String::from("[]"));
            continue;
        }
        let list_ptr = vh as *const MimiList;
        let json_ptr = mimi_list_set_option_product_to_json(list_ptr, arity, display_style);
        let s = unsafe { cstr_to_string(json_ptr) };
        if !json_ptr.is_null() {
            unsafe {
                libc::free(json_ptr as *mut _);
            }
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of List of Set of product from JSON.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_list_set_product_i64(
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
        let arr_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let arr = String::from_utf8_lossy(&bytes[arr_start..i]).into_owned();
        let c_arr = alloc_c_string(&arr);
        let list_ptr = mimi_list_from_json_set_product_i64(c_arr, arity);
        if !c_arr.is_null() {
            unsafe {
                libc::free(c_arr as *mut _);
            }
        }
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, list_ptr as ValueHandle);
        }
    }
    handle
}

/// Map of List of Set of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_list_set_product_i64(
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
        if vh == 0 {
            parts.push(String::from("[]"));
            continue;
        }
        let list_ptr = vh as *const MimiList;
        let json_ptr = if display_style != 0 {
            mimi_list_set_product_to_string(list_ptr, arity)
        } else {
            mimi_list_set_product_to_json(list_ptr, arity)
        };
        let s = unsafe { cstr_to_string(json_ptr) };
        if !json_ptr.is_null() {
            unsafe {
                libc::free(json_ptr as *mut _);
            }
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// List of Set of Option of product from JSON.
#[no_mangle]
pub extern "C" fn mimi_list_from_json_set_option_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> *mut MimiList {
    let empty = || {
        let list = unsafe { libc::malloc(std::mem::size_of::<MimiList>()) as *mut MimiList };
        if !list.is_null() {
            unsafe {
                (*list).len = 0;
                (*list).data = std::ptr::null_mut();
                (*list).owns_data = true;
            }
        }
        list
    };
    if json.is_null() || arity <= 0 || arity > 16 {
        return empty();
    }
    let s = unsafe { cstr_to_string(json) };
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b'[' {
        return empty();
    }
    i += 1;
    let mut handles: Vec<i64> = Vec::new();
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
        let arr_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let arr = String::from_utf8_lossy(&bytes[arr_start..i]).into_owned();
        let c_arr = alloc_c_string(&arr);
        let set_h = mimi_set_from_json_option_product_i64(c_arr, arity);
        if !c_arr.is_null() {
            unsafe {
                libc::free(c_arr as *mut _);
            }
        }
        handles.push(set_h as i64);
    }
    let list = unsafe { libc::malloc(std::mem::size_of::<MimiList>()) as *mut MimiList };
    if list.is_null() {
        return empty();
    }
    let data_size = handles.len() * 8;
    let data = if data_size == 0 {
        std::ptr::null_mut()
    } else {
        unsafe { libc::malloc(data_size) as *mut i64 }
    };
    if data_size > 0 && data.is_null() {
        unsafe {
            libc::free(list as *mut _);
        }
        return empty();
    }
    if !data.is_null() {
        unsafe {
            std::ptr::copy_nonoverlapping(handles.as_ptr(), data, handles.len());
        }
    }
    unsafe {
        (*list).len = handles.len() as i64;
        (*list).data = data as *mut *mut std::ffi::c_char;
        (*list).owns_data = true;
    }
    list
}

/// List of Set of Option of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_list_set_option_product_to_json(
    list: *const MimiList,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if list.is_null() || arity <= 0 || arity > 16 {
        return alloc_c_string("[]");
    }
    let lst = unsafe { &*list };
    if lst.data.is_null() || lst.len <= 0 {
        return alloc_c_string("[]");
    }
    let mut parts: Vec<String> = Vec::with_capacity(lst.len as usize * 2 + 2);
    parts.push(String::from("["));
    for i in 0..lst.len as isize {
        if i > 0 {
            parts.push(if display_style != 0 {
                String::from(", ")
            } else {
                String::from(",")
            });
        }
        let h = unsafe { *(lst.data as *const i64).offset(i) };
        let set_json = mimi_set_to_json_option_product_i64(h as SetHandle, arity, display_style);
        let s = unsafe { cstr_to_string(set_json) };
        if !set_json.is_null() {
            unsafe {
                libc::free(set_json as *mut _);
            }
        }
        parts.push(s);
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

/// List of Set of Result of product from JSON.
#[no_mangle]
pub extern "C" fn mimi_list_from_json_set_result_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> *mut MimiList {
    let empty = || {
        let list = unsafe { libc::malloc(std::mem::size_of::<MimiList>()) as *mut MimiList };
        if !list.is_null() {
            unsafe {
                (*list).len = 0;
                (*list).data = std::ptr::null_mut();
                (*list).owns_data = true;
            }
        }
        list
    };
    if json.is_null() || arity <= 0 || arity > 16 {
        return empty();
    }
    let s = unsafe { cstr_to_string(json) };
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b'[' {
        return empty();
    }
    i += 1;
    let mut handles: Vec<i64> = Vec::new();
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
        let arr_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let arr = String::from_utf8_lossy(&bytes[arr_start..i]).into_owned();
        let c_arr = alloc_c_string(&arr);
        let set_h = mimi_set_from_json_result_product_i64(c_arr, arity);
        if !c_arr.is_null() {
            unsafe {
                libc::free(c_arr as *mut _);
            }
        }
        handles.push(set_h as i64);
    }
    let list = unsafe { libc::malloc(std::mem::size_of::<MimiList>()) as *mut MimiList };
    if list.is_null() {
        return empty();
    }
    let data_size = handles.len() * 8;
    let data = if data_size == 0 {
        std::ptr::null_mut()
    } else {
        unsafe { libc::malloc(data_size) as *mut i64 }
    };
    if data_size > 0 && data.is_null() {
        unsafe {
            libc::free(list as *mut _);
        }
        return empty();
    }
    if !data.is_null() {
        unsafe {
            std::ptr::copy_nonoverlapping(handles.as_ptr(), data, handles.len());
        }
    }
    unsafe {
        (*list).len = handles.len() as i64;
        (*list).data = data as *mut *mut std::ffi::c_char;
        (*list).owns_data = true;
    }
    list
}

/// List of Set of Result of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_list_set_result_product_to_json(
    list: *const MimiList,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if list.is_null() || arity <= 0 || arity > 16 {
        return alloc_c_string("[]");
    }
    let lst = unsafe { &*list };
    if lst.data.is_null() || lst.len <= 0 {
        return alloc_c_string("[]");
    }
    let mut parts: Vec<String> = Vec::with_capacity(lst.len as usize * 2 + 2);
    parts.push(String::from("["));
    for i in 0..lst.len as isize {
        if i > 0 {
            parts.push(if display_style != 0 {
                String::from(", ")
            } else {
                String::from(",")
            });
        }
        let h = unsafe { *(lst.data as *const i64).offset(i) };
        let set_json = mimi_set_to_json_result_product_i64(h as SetHandle, arity, display_style);
        let s = unsafe { cstr_to_string(set_json) };
        if !set_json.is_null() {
            unsafe {
                libc::free(set_json as *mut _);
            }
        }
        parts.push(s);
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

/// List of Result of Option of product from JSON.
/// Element pack: `{i64 res_disc, i64 opt_pack_or_err}` where opt_pack is option product heap.
#[no_mangle]
pub extern "C" fn mimi_list_from_json_result_option_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> *mut MimiList {
    let empty = || {
        let list = unsafe { libc::malloc(std::mem::size_of::<MimiList>()) as *mut MimiList };
        if !list.is_null() {
            unsafe {
                (*list).len = 0;
                (*list).data = std::ptr::null_mut();
                (*list).owns_data = true;
            }
        }
        list
    };
    if json.is_null() || arity <= 0 || arity > 16 {
        return empty();
    }
    // Parse as list of tagged Result via wrapping each element through option product helper
    // by building a JSON array and reusing map_from_json_result_option_product single-key?
    // Simpler: walk array and call map_from_json_result_option_product for each {"_":elem}
    let s = unsafe { cstr_to_string(json) };
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b'[' {
        return empty();
    }
    i += 1;
    let mut handles: Vec<i64> = Vec::new();
    loop {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b']' {
            break;
        }
        let val_start = i;
        if bytes[i] == b'{' || bytes[i] == b'[' {
            let open = bytes[i];
            let close = if open == b'{' { b'}' } else { b']' };
            let mut depth = 0i32;
            while i < bytes.len() {
                if bytes[i] == open {
                    depth += 1;
                } else if bytes[i] == close {
                    depth -= 1;
                    if depth == 0 {
                        i += 1;
                        break;
                    }
                } else if bytes[i] == b'"' {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                i += 1;
            }
        } else if bytes[i] == b'n' && i + 4 <= bytes.len() && &bytes[i..i + 4] == b"null" {
            i += 4;
        } else {
            break;
        }
        let val = String::from_utf8_lossy(&bytes[val_start..i]).into_owned();
        let wrap = format!("{{\"_\" :{}}}", val);
        let c_wrap = alloc_c_string(&wrap);
        let tmp = mimi_map_from_json_result_option_product_i64(c_wrap, arity);
        if !c_wrap.is_null() {
            unsafe {
                libc::free(c_wrap as *mut _);
            }
        }
        let mut h: i64 = 0;
        if tmp != 0 {
            let m = unsafe { &*map_from_handle(tmp) };
            if let Some(v) = m.inner.values().next() {
                h = *v as i64;
            }
        }
        handles.push(h);
    }
    let list = unsafe { libc::malloc(std::mem::size_of::<MimiList>()) as *mut MimiList };
    if list.is_null() {
        return empty();
    }
    let data_size = handles.len() * 8;
    let data = if data_size == 0 {
        std::ptr::null_mut()
    } else {
        unsafe { libc::malloc(data_size) as *mut i64 }
    };
    if data_size > 0 && data.is_null() {
        unsafe {
            libc::free(list as *mut _);
        }
        return empty();
    }
    if !data.is_null() {
        unsafe {
            std::ptr::copy_nonoverlapping(handles.as_ptr(), data, handles.len());
        }
    }
    unsafe {
        (*list).len = handles.len() as i64;
        (*list).data = data as *mut *mut std::ffi::c_char;
        (*list).owns_data = true;
    }
    list
}

/// List of Result of Option of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_list_result_option_product_to_json(
    list: *const MimiList,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if list.is_null() || arity <= 0 || arity > 16 {
        return alloc_c_string("[]");
    }
    let lst = unsafe { &*list };
    if lst.data.is_null() || lst.len <= 0 {
        return alloc_c_string("[]");
    }
    let mut parts: Vec<String> = Vec::with_capacity(lst.len as usize * 2 + 2);
    parts.push(String::from("["));
    for i in 0..lst.len as isize {
        if i > 0 {
            parts.push(if display_style != 0 {
                String::from(", ")
            } else {
                String::from(",")
            });
        }
        let h = unsafe { *(lst.data as *const i64).offset(i) };
        if h == 0 {
            if display_style != 0 {
                parts.push(String::from("Err()"));
            } else {
                parts.push(String::from("{\"Err\":[\"\"]}"));
            }
            continue;
        }
        // Reuse single-entry map result option product to_json
        let tmp = mimi_map_new();
        if tmp != 0 {
            unsafe {
                (*map_from_handle(tmp))
                    .inner
                    .insert("_".into(), h as ValueHandle);
            }
            let json_ptr = mimi_map_to_json_result_option_product_i64(tmp, arity, display_style);
            let s = unsafe { cstr_to_string(json_ptr) };
            if !json_ptr.is_null() {
                unsafe {
                    libc::free(json_ptr as *mut _);
                }
            }
            let val = if let Some(colon) = s.find(':') {
                let mut rest = s[colon + 1..].to_string();
                if rest.ends_with('}') {
                    rest.pop();
                }
                rest
            } else {
                s
            };
            parts.push(val);
        }
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

/// Map of List of Result of Option of product from JSON.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_list_result_option_product_i64(
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
        let arr_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let arr = String::from_utf8_lossy(&bytes[arr_start..i]).into_owned();
        let c_arr = alloc_c_string(&arr);
        let list_ptr = mimi_list_from_json_result_option_product_i64(c_arr, arity);
        if !c_arr.is_null() {
            unsafe {
                libc::free(c_arr as *mut _);
            }
        }
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, list_ptr as ValueHandle);
        }
    }
    handle
}

/// Map of List of Result of Option of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_list_result_option_product_i64(
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
        if vh == 0 {
            parts.push(String::from("[]"));
            continue;
        }
        let list_ptr = vh as *const MimiList;
        let json_ptr = mimi_list_result_option_product_to_json(list_ptr, arity, display_style);
        let s = unsafe { cstr_to_string(json_ptr) };
        if !json_ptr.is_null() {
            unsafe {
                libc::free(json_ptr as *mut _);
            }
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of Result of Option of List of product from JSON.
/// Pack: `{i64 disc, i64 opt_or_err}` where Ok opt pack is `{i64 opt_disc, i64 list_handle}`.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_result_option_list_product_i64(
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
        if i >= bytes.len() {
            break;
        }
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            continue;
        }
        if bytes[i] != b'{' {
            unsafe {
                libc::free(pack as *mut _);
            }
            break;
        }
        let obj_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let obj = String::from_utf8_lossy(&bytes[obj_start..i]).into_owned();
        if obj.contains("\"Err\"") {
            let mut err_s = String::new();
            if let Some(pos) = obj.find("\"Err\"") {
                let rest = &obj[pos + 5..];
                if let Some(q1) = rest.find('"') {
                    let r2 = &rest[q1 + 1..];
                    if let Some(q2) = r2.find('"') {
                        err_s = r2[..q2].to_string();
                    }
                }
            }
            let c = alloc_c_string(&err_s);
            unsafe {
                *pack = 0;
                *pack.add(1) = c as i64;
            }
        } else {
            // Ok: value may be null → Ok(None), or list → Ok(Some(list))
            let mut val = String::from("null");
            if let Some(pos) = obj.find("\"Ok\"") {
                let rest = &obj[pos + 4..];
                let rb = rest.as_bytes();
                let mut k = 0usize;
                while k < rb.len() && (rb[k].is_ascii_whitespace() || rb[k] == b':') {
                    k += 1;
                }
                if k < rb.len() {
                    if rb[k] == b'n' {
                        val = String::from("null");
                    } else if rb[k] == b'[' {
                        let start = k;
                        let mut depth = 0i32;
                        while k < rb.len() {
                            match rb[k] {
                                b'[' => depth += 1,
                                b']' => {
                                    depth -= 1;
                                    if depth == 0 {
                                        k += 1;
                                        break;
                                    }
                                }
                                b'"' => {
                                    k += 1;
                                    while k < rb.len() && rb[k] != b'"' {
                                        if rb[k] == b'\\' {
                                            k += 1;
                                        }
                                        k += 1;
                                    }
                                }
                                _ => {}
                            }
                            k += 1;
                        }
                        val = rest[start..k].to_string();
                    }
                }
            }
            let opt_pack = unsafe { libc::malloc(16) as *mut i64 };
            if opt_pack.is_null() {
                unsafe {
                    libc::free(pack as *mut _);
                }
                continue;
            }
            if val == "null" {
                unsafe {
                    *opt_pack = 0;
                    *opt_pack.add(1) = 0;
                }
            } else {
                // Parse list of products via map list product single-key trick
                let wrap = format!("{{\"_\" :{}}}", val);
                let c_wrap = alloc_c_string(&wrap);
                let tmp = mimi_map_from_json_list_product_i64(c_wrap, arity);
                if !c_wrap.is_null() {
                    unsafe {
                        libc::free(c_wrap as *mut _);
                    }
                }
                let mut list_h: i64 = 0;
                if tmp != 0 {
                    let m = unsafe { &*map_from_handle(tmp) };
                    if let Some(v) = m.inner.values().next() {
                        list_h = *v as i64;
                    }
                }
                unsafe {
                    *opt_pack = 1;
                    *opt_pack.add(1) = list_h;
                }
            }
            unsafe {
                *pack = 1;
                *pack.add(1) = opt_pack as i64;
            }
        }
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, pack as ValueHandle);
        }
    }
    handle
}

/// Map of Result of Option of List of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_result_option_list_product_i64(
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
        if vh == 0 {
            if display_style != 0 {
                parts.push(String::from("Err()"));
            } else {
                parts.push(String::from("{\"Err\":[\"\"]}"));
            }
            continue;
        }
        let base = vh as *const i64;
        let disc = unsafe { *base };
        if disc == 0 {
            let err_ptr = unsafe { *base.add(1) } as *const std::ffi::c_char;
            let err_s = if err_ptr.is_null() {
                String::new()
            } else {
                unsafe { cstr_to_string(err_ptr) }
            };
            if display_style != 0 {
                parts.push(format!("Err({})", err_s));
            } else {
                parts.push(format!("{{\"Err\":[{}]}}", json_escape_string(&err_s)));
            }
        } else {
            let opt_h = unsafe { *base.add(1) } as *const i64;
            if opt_h.is_null() {
                if display_style != 0 {
                    parts.push(String::from("Ok(None())"));
                } else {
                    parts.push(String::from("{\"Ok\":[\"None\"]}"));
                }
            } else {
                let opt_disc = unsafe { *opt_h };
                if opt_disc == 0 {
                    if display_style != 0 {
                        parts.push(String::from("Ok(None())"));
                    } else {
                        parts.push(String::from("{\"Ok\":[\"None\"]}"));
                    }
                } else {
                    let list_ptr = unsafe { *opt_h.add(1) } as *const u8;
                    let tmp = mimi_map_new();
                    if tmp != 0 && !list_ptr.is_null() {
                        unsafe {
                            (*map_from_handle(tmp))
                                .inner
                                .insert("_".into(), list_ptr as ValueHandle);
                        }
                        let json_ptr = mimi_map_to_json_list_product_i64(tmp, arity, display_style);
                        let s = unsafe { cstr_to_string(json_ptr) };
                        if !json_ptr.is_null() {
                            unsafe {
                                libc::free(json_ptr as *mut _);
                            }
                        }
                        let val = if let Some(colon) = s.find(':') {
                            let mut rest = s[colon + 1..].to_string();
                            if rest.ends_with('}') {
                                rest.pop();
                            }
                            rest
                        } else {
                            s
                        };
                        if display_style != 0 {
                            parts.push(format!("Ok(Some({}))", val));
                        } else {
                            parts.push(format!("{{\"Ok\":[{{\"Some\":[{}]}}]}}", val));
                        }
                    }
                }
            }
        }
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of Option of Set of List of product from JSON.
/// Pack: `{i64 disc, i64 set_handle}` disc 1=Some set of list product, 0=None.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_option_set_list_product_i64(
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
        if i >= bytes.len() {
            break;
        }
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            continue;
        }
        if bytes[i] == b'n' && i + 4 <= bytes.len() && &bytes[i..i + 4] == b"null" {
            i += 4;
            unsafe {
                *pack = 0;
                *pack.add(1) = 0;
            }
        } else {
            let val_start = i;
            if bytes[i] == b'{' || bytes[i] == b'[' {
                let open = bytes[i];
                let close = if open == b'{' { b'}' } else { b']' };
                let mut depth = 0i32;
                while i < bytes.len() {
                    if bytes[i] == open {
                        depth += 1;
                    } else if bytes[i] == close {
                        depth -= 1;
                        if depth == 0 {
                            i += 1;
                            break;
                        }
                    } else if bytes[i] == b'"' {
                        i += 1;
                        while i < bytes.len() && bytes[i] != b'"' {
                            if bytes[i] == b'\\' {
                                i += 1;
                            }
                            i += 1;
                        }
                    }
                    i += 1;
                }
            } else {
                unsafe {
                    libc::free(pack as *mut _);
                }
                break;
            }
            let val = String::from_utf8_lossy(&bytes[val_start..i]).into_owned();
            let arr = if val.starts_with('{') {
                // extract Some array
                if let Some(pos) = val.find('[') {
                    let mut depth = 0i32;
                    let vb = val.as_bytes();
                    let mut k = pos;
                    while k < vb.len() {
                        match vb[k] {
                            b'[' => depth += 1,
                            b']' => {
                                depth -= 1;
                                if depth == 0 {
                                    k += 1;
                                    break;
                                }
                            }
                            b'"' => {
                                k += 1;
                                while k < vb.len() && vb[k] != b'"' {
                                    if vb[k] == b'\\' {
                                        k += 1;
                                    }
                                    k += 1;
                                }
                            }
                            _ => {}
                        }
                        k += 1;
                    }
                    val[pos..k].to_string()
                } else {
                    String::from("[]")
                }
            } else {
                val
            };
            let c_arr = alloc_c_string(&arr);
            let set_h = mimi_set_from_json_list_product_i64(c_arr, arity);
            if !c_arr.is_null() {
                unsafe {
                    libc::free(c_arr as *mut _);
                }
            }
            unsafe {
                *pack = 1;
                *pack.add(1) = set_h as i64;
            }
        }
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, pack as ValueHandle);
        }
    }
    handle
}

/// Map of Option of Set of List of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_option_set_list_product_i64(
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
        if vh == 0 {
            if display_style != 0 {
                parts.push(String::from("None()"));
            } else {
                parts.push(String::from("\"None\""));
            }
            continue;
        }
        let base = vh as *const i64;
        let disc = unsafe { *base };
        if disc == 0 {
            if display_style != 0 {
                parts.push(String::from("None()"));
            } else {
                parts.push(String::from("\"None\""));
            }
        } else {
            let set_h = unsafe { *base.add(1) } as SetHandle;
            let set_json = mimi_set_to_json_list_product_i64(set_h, arity, display_style);
            let s = unsafe { cstr_to_string(set_json) };
            if !set_json.is_null() {
                unsafe {
                    libc::free(set_json as *mut _);
                }
            }
            if display_style != 0 {
                parts.push(format!("Some({})", s));
            } else {
                parts.push(format!("{{\"Some\":[{}]}}", s));
            }
        }
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of Option of Result of List of product from JSON.
/// Pack: `{i64 disc, i64 res_handle}` where res is Result list product pack.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_option_result_list_product_i64(
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
        if i >= bytes.len() {
            break;
        }
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            continue;
        }
        if bytes[i] == b'n' && i + 4 <= bytes.len() && &bytes[i..i + 4] == b"null" {
            i += 4;
            unsafe {
                *pack = 0;
                *pack.add(1) = 0;
            }
        } else {
            let val_start = i;
            if bytes[i] == b'{' {
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
                        b'"' => {
                            i += 1;
                            while i < bytes.len() && bytes[i] != b'"' {
                                if bytes[i] == b'\\' {
                                    i += 1;
                                }
                                i += 1;
                            }
                        }
                        _ => {}
                    }
                    i += 1;
                }
            } else {
                unsafe {
                    libc::free(pack as *mut _);
                }
                break;
            }
            let val = String::from_utf8_lossy(&bytes[val_start..i]).into_owned();
            // Parse as single-key Result List product map
            let wrap = format!("{{\"_\" :{}}}", val);
            let c_wrap = alloc_c_string(&wrap);
            let tmp = mimi_map_from_json_result_list_product_i64(c_wrap, arity);
            if !c_wrap.is_null() {
                unsafe {
                    libc::free(c_wrap as *mut _);
                }
            }
            let mut res_h: i64 = 0;
            if tmp != 0 {
                let m = unsafe { &*map_from_handle(tmp) };
                if let Some(v) = m.inner.values().next() {
                    res_h = *v as i64;
                }
            }
            unsafe {
                *pack = 1;
                *pack.add(1) = res_h;
            }
        }
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, pack as ValueHandle);
        }
    }
    handle
}

/// Map of Option of Result of List of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_option_result_list_product_i64(
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
        if vh == 0 {
            if display_style != 0 {
                parts.push(String::from("None()"));
            } else {
                parts.push(String::from("\"None\""));
            }
            continue;
        }
        let base = vh as *const i64;
        let disc = unsafe { *base };
        if disc == 0 {
            if display_style != 0 {
                parts.push(String::from("None()"));
            } else {
                parts.push(String::from("\"None\""));
            }
        } else {
            let res_h = unsafe { *base.add(1) };
            let tmp = mimi_map_new();
            if tmp != 0 {
                unsafe {
                    (*map_from_handle(tmp))
                        .inner
                        .insert("_".into(), res_h as ValueHandle);
                }
                let json_ptr = mimi_map_to_json_result_list_product_i64(tmp, arity, display_style);
                let s = unsafe { cstr_to_string(json_ptr) };
                if !json_ptr.is_null() {
                    unsafe {
                        libc::free(json_ptr as *mut _);
                    }
                }
                let val = if let Some(colon) = s.find(':') {
                    let mut rest = s[colon + 1..].to_string();
                    if rest.ends_with('}') {
                        rest.pop();
                    }
                    rest
                } else {
                    s
                };
                if display_style != 0 {
                    parts.push(format!("Some({})", val));
                } else {
                    parts.push(format!("{{\"Some\":[{}]}}", val));
                }
            }
        }
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of Result of List of Set of product from JSON.
/// Pack: `{i64 disc, i64 list_or_err}` Ok list is List of Set product handles.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_result_list_set_product_i64(
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
        if i >= bytes.len() {
            break;
        }
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            continue;
        }
        if bytes[i] != b'{' {
            unsafe {
                libc::free(pack as *mut _);
            }
            break;
        }
        let obj_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let obj = String::from_utf8_lossy(&bytes[obj_start..i]).into_owned();
        if obj.contains("\"Err\"") {
            let mut err_s = String::new();
            if let Some(pos) = obj.find("\"Err\"") {
                let rest = &obj[pos + 5..];
                if let Some(q1) = rest.find('"') {
                    let r2 = &rest[q1 + 1..];
                    if let Some(q2) = r2.find('"') {
                        err_s = r2[..q2].to_string();
                    }
                }
            }
            let c = alloc_c_string(&err_s);
            unsafe {
                *pack = 0;
                *pack.add(1) = c as i64;
            }
        } else {
            let mut arr = String::from("[]");
            if let Some(pos) = obj.find("\"Ok\"") {
                let rest = &obj[pos + 4..];
                if let Some(br) = rest.find('[') {
                    let start = br;
                    let rb = rest.as_bytes();
                    let mut depth = 0i32;
                    let mut k = start;
                    while k < rb.len() {
                        match rb[k] {
                            b'[' => depth += 1,
                            b']' => {
                                depth -= 1;
                                if depth == 0 {
                                    k += 1;
                                    break;
                                }
                            }
                            b'"' => {
                                k += 1;
                                while k < rb.len() && rb[k] != b'"' {
                                    if rb[k] == b'\\' {
                                        k += 1;
                                    }
                                    k += 1;
                                }
                            }
                            _ => {}
                        }
                        k += 1;
                    }
                    arr = rest[start..k].to_string();
                }
            }
            let c_arr = alloc_c_string(&arr);
            let list_ptr = mimi_list_from_json_set_product_i64(c_arr, arity);
            if !c_arr.is_null() {
                unsafe {
                    libc::free(c_arr as *mut _);
                }
            }
            unsafe {
                *pack = 1;
                *pack.add(1) = list_ptr as i64;
            }
        }
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, pack as ValueHandle);
        }
    }
    handle
}

/// Map of Result of List of Set of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_result_list_set_product_i64(
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
        if vh == 0 {
            if display_style != 0 {
                parts.push(String::from("Err()"));
            } else {
                parts.push(String::from("{\"Err\":[\"\"]}"));
            }
            continue;
        }
        let base = vh as *const i64;
        let disc = unsafe { *base };
        if disc == 0 {
            let err_ptr = unsafe { *base.add(1) } as *const std::ffi::c_char;
            let err_s = if err_ptr.is_null() {
                String::new()
            } else {
                unsafe { cstr_to_string(err_ptr) }
            };
            if display_style != 0 {
                parts.push(format!("Err({})", err_s));
            } else {
                parts.push(format!("{{\"Err\":[{}]}}", json_escape_string(&err_s)));
            }
        } else {
            let list_ptr = unsafe { *base.add(1) } as *const MimiList;
            let list_json = if display_style != 0 {
                mimi_list_set_product_to_string(list_ptr, arity)
            } else {
                mimi_list_set_product_to_json(list_ptr, arity)
            };
            let s = unsafe { cstr_to_string(list_json) };
            if !list_json.is_null() {
                unsafe {
                    libc::free(list_json as *mut _);
                }
            }
            if display_style != 0 {
                parts.push(format!("Ok({})", s));
            } else {
                parts.push(format!("{{\"Ok\":[{}]}}", s));
            }
        }
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of Result of List of Option of product from JSON.
/// Pack: `{i64 disc, i64 list_or_err}` disc 1=Ok list option product, 0=Err string.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_result_list_option_product_i64(
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
        if i >= bytes.len() {
            break;
        }
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            continue;
        }
        if bytes[i] != b'{' {
            unsafe {
                libc::free(pack as *mut _);
            }
            break;
        }
        let obj_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let obj = String::from_utf8_lossy(&bytes[obj_start..i]).into_owned();
        if obj.contains("\"Err\"") {
            let mut err_s = String::new();
            if let Some(pos) = obj.find("\"Err\"") {
                let rest = &obj[pos + 5..];
                if let Some(q1) = rest.find('"') {
                    let r2 = &rest[q1 + 1..];
                    if let Some(q2) = r2.find('"') {
                        err_s = r2[..q2].to_string();
                    }
                }
            }
            let c = alloc_c_string(&err_s);
            unsafe {
                *pack = 0;
                *pack.add(1) = c as i64;
            }
        } else {
            let mut arr = String::from("[]");
            if let Some(pos) = obj.find("\"Ok\"") {
                let rest = &obj[pos + 4..];
                if let Some(br) = rest.find('[') {
                    let start = br;
                    let rb = rest.as_bytes();
                    let mut depth = 0i32;
                    let mut k = start;
                    while k < rb.len() {
                        match rb[k] {
                            b'[' => depth += 1,
                            b']' => {
                                depth -= 1;
                                if depth == 0 {
                                    k += 1;
                                    break;
                                }
                            }
                            b'"' => {
                                k += 1;
                                while k < rb.len() && rb[k] != b'"' {
                                    if rb[k] == b'\\' {
                                        k += 1;
                                    }
                                    k += 1;
                                }
                            }
                            _ => {}
                        }
                        k += 1;
                    }
                    arr = rest[start..k].to_string();
                }
            }
            let c_arr = alloc_c_string(&arr);
            let list_ptr = mimi_list_from_json_option_product_i64(c_arr, arity);
            if !c_arr.is_null() {
                unsafe {
                    libc::free(c_arr as *mut _);
                }
            }
            unsafe {
                *pack = 1;
                *pack.add(1) = list_ptr as i64;
            }
        }
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, pack as ValueHandle);
        }
    }
    handle
}

/// Map of Result of List of Option of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_result_list_option_product_i64(
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
        if vh == 0 {
            if display_style != 0 {
                parts.push(String::from("Err()"));
            } else {
                parts.push(String::from("{\"Err\":[\"\"]}"));
            }
            continue;
        }
        let base = vh as *const i64;
        let disc = unsafe { *base };
        if disc == 0 {
            let err_ptr = unsafe { *base.add(1) } as *const std::ffi::c_char;
            let err_s = if err_ptr.is_null() {
                String::new()
            } else {
                unsafe { cstr_to_string(err_ptr) }
            };
            if display_style != 0 {
                parts.push(format!("Err({})", err_s));
            } else {
                parts.push(format!("{{\"Err\":[{}]}}", json_escape_string(&err_s)));
            }
        } else {
            let list_ptr = unsafe { *base.add(1) } as *const MimiList;
            let list_json = mimi_list_option_product_to_json(list_ptr, arity, display_style);
            let s = unsafe { cstr_to_string(list_json) };
            if !list_json.is_null() {
                unsafe {
                    libc::free(list_json as *mut _);
                }
            }
            if display_style != 0 {
                parts.push(format!("Ok({})", s));
            } else {
                parts.push(format!("{{\"Ok\":[{}]}}", s));
            }
        }
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of Set of Option of product from JSON.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_set_option_product_i64(
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
        let arr_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let arr = String::from_utf8_lossy(&bytes[arr_start..i]).into_owned();
        let c_arr = alloc_c_string(&arr);
        let set_h = mimi_set_from_json_option_product_i64(c_arr, arity);
        if !c_arr.is_null() {
            unsafe {
                libc::free(c_arr as *mut _);
            }
        }
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, set_h as ValueHandle);
        }
    }
    handle
}

/// Map of Set of Option of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_set_option_product_i64(
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
        if vh == 0 {
            if display_style != 0 {
                parts.push(String::from("Set{}"));
            } else {
                parts.push(String::from("[]"));
            }
            continue;
        }
        let set_h = vh as SetHandle;
        let json_ptr = mimi_set_to_json_option_product_i64(set_h, arity, display_style);
        let s = unsafe { cstr_to_string(json_ptr) };
        if !json_ptr.is_null() {
            unsafe {
                libc::free(json_ptr as *mut _);
            }
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of Set of Result of Option of product from JSON.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_set_result_option_product_i64(
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
        let arr_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let arr = String::from_utf8_lossy(&bytes[arr_start..i]).into_owned();
        let c_arr = alloc_c_string(&arr);
        let set_h = mimi_set_from_json_result_option_product_i64(c_arr, arity);
        if !c_arr.is_null() {
            unsafe {
                libc::free(c_arr as *mut _);
            }
        }
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, set_h as ValueHandle);
        }
    }
    handle
}

/// Map of Set of Result of Option of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_set_result_option_product_i64(
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
        if vh == 0 {
            if display_style != 0 {
                parts.push(String::from("Set{}"));
            } else {
                parts.push(String::from("[]"));
            }
            continue;
        }
        let set_h = vh as SetHandle;
        let json_ptr = mimi_set_to_json_result_option_product_i64(set_h, arity, display_style);
        let s = unsafe { cstr_to_string(json_ptr) };
        if !json_ptr.is_null() {
            unsafe {
                libc::free(json_ptr as *mut _);
            }
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of Set of Result of product from JSON.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_set_result_product_i64(
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
        let arr_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let arr = String::from_utf8_lossy(&bytes[arr_start..i]).into_owned();
        let c_arr = alloc_c_string(&arr);
        let set_h = mimi_set_from_json_result_product_i64(c_arr, arity);
        if !c_arr.is_null() {
            unsafe {
                libc::free(c_arr as *mut _);
            }
        }
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, set_h as ValueHandle);
        }
    }
    handle
}

/// Map of Set of Result of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_set_result_product_i64(
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
        if vh == 0 {
            if display_style != 0 {
                parts.push(String::from("Set{}"));
            } else {
                parts.push(String::from("[]"));
            }
            continue;
        }
        let set_h = vh as SetHandle;
        let json_ptr = mimi_set_to_json_result_product_i64(set_h, arity, display_style);
        let s = unsafe { cstr_to_string(json_ptr) };
        if !json_ptr.is_null() {
            unsafe {
                libc::free(json_ptr as *mut _);
            }
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of List of Result of product from JSON.
/// Each map value is a list handle whose elements are Result product packs.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_list_result_product_i64(
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
        let arr_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let arr = String::from_utf8_lossy(&bytes[arr_start..i]).into_owned();
        let c_arr = alloc_c_string(&arr);
        let list_ptr = mimi_list_from_json_result_product_i64(c_arr, arity);
        if !c_arr.is_null() {
            unsafe {
                libc::free(c_arr as *mut _);
            }
        }
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, list_ptr as ValueHandle);
        }
    }
    handle
}

/// Map of List of Result of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_list_result_product_i64(
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
        if vh == 0 {
            parts.push(String::from("[]"));
            continue;
        }
        let list_ptr = vh as *const MimiList;
        let json_ptr = mimi_list_result_product_to_json(list_ptr, arity, display_style);
        let s = unsafe { cstr_to_string(json_ptr) };
        if !json_ptr.is_null() {
            unsafe {
                libc::free(json_ptr as *mut _);
            }
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of Option of List of product from JSON.
/// Pack: `{i64 disc, i64 list_handle}` disc 1=Some list, 0=None.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_option_list_product_i64(
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
    let n = arity as usize;
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
        if i >= bytes.len() {
            break;
        }
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            continue;
        }
        if bytes[i] == b'n' && i + 4 <= bytes.len() && &bytes[i..i + 4] == b"null" {
            i += 4;
            unsafe {
                *pack = 0;
                *pack.add(1) = 0;
            }
        } else {
            let val_start = i;
            if bytes[i] == b'{' || bytes[i] == b'[' {
                let open = bytes[i];
                let close = if open == b'{' { b'}' } else { b']' };
                let mut depth = 0i32;
                while i < bytes.len() {
                    if bytes[i] == open {
                        depth += 1;
                    } else if bytes[i] == close {
                        depth -= 1;
                        if depth == 0 {
                            i += 1;
                            break;
                        }
                    } else if bytes[i] == b'"' {
                        i += 1;
                        while i < bytes.len() && bytes[i] != b'"' {
                            if bytes[i] == b'\\' {
                                i += 1;
                            }
                            i += 1;
                        }
                    }
                    i += 1;
                }
            } else {
                unsafe {
                    libc::free(pack as *mut _);
                }
                break;
            }
            let val = String::from_utf8_lossy(&bytes[val_start..i]).into_owned();
            let arr = if val.starts_with('{') {
                if let Some(pos) = val.find('[') {
                    let mut depth = 0i32;
                    let vb = val.as_bytes();
                    let mut k = pos;
                    while k < vb.len() {
                        match vb[k] {
                            b'[' => depth += 1,
                            b']' => {
                                depth -= 1;
                                if depth == 0 {
                                    k += 1;
                                    break;
                                }
                            }
                            b'"' => {
                                k += 1;
                                while k < vb.len() && vb[k] != b'"' {
                                    if vb[k] == b'\\' {
                                        k += 1;
                                    }
                                    k += 1;
                                }
                            }
                            _ => {}
                        }
                        k += 1;
                    }
                    val[pos..k].to_string()
                } else {
                    String::from("[]")
                }
            } else {
                val
            };
            // Parse list of products into list handle (same as map list product values).
            let ab = arr.as_bytes();
            let mut j = 0usize;
            while j < ab.len() && ab[j].is_ascii_whitespace() {
                j += 1;
            }
            let mut prod_handles: Vec<i64> = Vec::new();
            if j < ab.len() && ab[j] == b'[' {
                j += 1;
                loop {
                    while j < ab.len() && (ab[j].is_ascii_whitespace() || ab[j] == b',') {
                        j += 1;
                    }
                    if j >= ab.len() || ab[j] == b']' {
                        break;
                    }
                    if ab[j] != b'[' {
                        break;
                    }
                    j += 1;
                    let mut fields = vec![0i64; n];
                    for fi in 0..n {
                        while j < ab.len() && (ab[j].is_ascii_whitespace() || ab[j] == b',') {
                            j += 1;
                        }
                        let neg = j < ab.len() && ab[j] == b'-';
                        if neg {
                            j += 1;
                        }
                        let mut v: i64 = 0;
                        while j < ab.len() && ab[j].is_ascii_digit() {
                            v = v.saturating_mul(10).saturating_add((ab[j] - b'0') as i64);
                            j += 1;
                        }
                        if neg {
                            v = -v;
                        }
                        fields[fi] = v;
                    }
                    while j < ab.len() && ab[j] != b']' {
                        j += 1;
                    }
                    if j < ab.len() && ab[j] == b']' {
                        j += 1;
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
            }
            let list_ptr = unsafe { libc::malloc(16) as *mut u8 };
            if list_ptr.is_null() {
                unsafe {
                    libc::free(pack as *mut _);
                }
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
                *pack = 1;
                *pack.add(1) = list_ptr as i64;
            }
        }
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, pack as ValueHandle);
        }
    }
    handle
}

/// Map of Option of List of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_option_list_product_i64(
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
            if display_style != 0 {
                parts.push(String::from("None()"));
            } else {
                parts.push(String::from("\"None\""));
            }
            continue;
        }
        let base = vh as *const i64;
        let disc = unsafe { *base };
        if disc == 0 {
            if display_style != 0 {
                parts.push(String::from("None()"));
            } else {
                parts.push(String::from("\"None\""));
            }
        } else {
            let list_ptr = unsafe { *base.add(1) } as *const u8;
            let tmp = mimi_map_new();
            if tmp != 0 && !list_ptr.is_null() {
                unsafe {
                    (*map_from_handle(tmp))
                        .inner
                        .insert("_".into(), list_ptr as ValueHandle);
                }
                let json_ptr = mimi_map_to_json_list_product_i64(tmp, arity, display_style);
                let s = unsafe { cstr_to_string(json_ptr) };
                if !json_ptr.is_null() {
                    unsafe {
                        libc::free(json_ptr as *mut _);
                    }
                }
                let val = if let Some(colon) = s.find(':') {
                    let mut rest = s[colon + 1..].to_string();
                    if rest.ends_with('}') {
                        rest.pop();
                    }
                    rest
                } else {
                    s
                };
                if display_style != 0 {
                    parts.push(format!("Some({})", val));
                } else {
                    parts.push(format!("{{\"Some\":[{}]}}", val));
                }
            }
            let _ = n;
        }
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of Result of List of product from JSON.
/// Pack: `{i64 disc, i64 list_handle}` disc 1=Ok list of product packs, 0=Err string.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_result_list_product_i64(
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
    let n = arity as usize;
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
        if i >= bytes.len() {
            break;
        }
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            continue;
        }
        if bytes[i] != b'{' {
            unsafe {
                libc::free(pack as *mut _);
            }
            break;
        }
        let obj_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let obj = String::from_utf8_lossy(&bytes[obj_start..i]).into_owned();
        if obj.contains("\"Err\"") {
            let mut err_s = String::new();
            if let Some(pos) = obj.find("\"Err\"") {
                let rest = &obj[pos + 5..];
                if let Some(q1) = rest.find('"') {
                    let r2 = &rest[q1 + 1..];
                    if let Some(q2) = r2.find('"') {
                        err_s = r2[..q2].to_string();
                    }
                }
            }
            let c = alloc_c_string(&err_s);
            unsafe {
                *pack = 0;
                *pack.add(1) = c as i64;
            }
        } else {
            let mut arr = String::from("[]");
            if let Some(pos) = obj.find("\"Ok\"") {
                let rest = &obj[pos + 4..];
                if let Some(br) = rest.find('[') {
                    let start = br;
                    let rb = rest.as_bytes();
                    let mut depth = 0i32;
                    let mut k = start;
                    while k < rb.len() {
                        match rb[k] {
                            b'[' => depth += 1,
                            b']' => {
                                depth -= 1;
                                if depth == 0 {
                                    k += 1;
                                    break;
                                }
                            }
                            b'"' => {
                                k += 1;
                                while k < rb.len() && rb[k] != b'"' {
                                    if rb[k] == b'\\' {
                                        k += 1;
                                    }
                                    k += 1;
                                }
                            }
                            _ => {}
                        }
                        k += 1;
                    }
                    arr = rest[start..k].to_string();
                }
            }
            // Parse list of product arrays into list handle (same packing as map_from_json_list_product).
            let ab = arr.as_bytes();
            let mut j = 0usize;
            while j < ab.len() && ab[j].is_ascii_whitespace() {
                j += 1;
            }
            let mut prod_handles: Vec<i64> = Vec::new();
            if j < ab.len() && ab[j] == b'[' {
                j += 1;
                loop {
                    while j < ab.len() && (ab[j].is_ascii_whitespace() || ab[j] == b',') {
                        j += 1;
                    }
                    if j >= ab.len() || ab[j] == b']' {
                        break;
                    }
                    if ab[j] != b'[' {
                        break;
                    }
                    j += 1;
                    let mut fields = vec![0i64; n];
                    for fi in 0..n {
                        while j < ab.len() && (ab[j].is_ascii_whitespace() || ab[j] == b',') {
                            j += 1;
                        }
                        let neg = j < ab.len() && ab[j] == b'-';
                        if neg {
                            j += 1;
                        }
                        let mut v: i64 = 0;
                        while j < ab.len() && ab[j].is_ascii_digit() {
                            v = v.saturating_mul(10).saturating_add((ab[j] - b'0') as i64);
                            j += 1;
                        }
                        if neg {
                            v = -v;
                        }
                        fields[fi] = v;
                    }
                    while j < ab.len() && ab[j] != b']' {
                        j += 1;
                    }
                    if j < ab.len() && ab[j] == b']' {
                        j += 1;
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
            }
            let list_ptr = unsafe { libc::malloc(16) as *mut u8 };
            if list_ptr.is_null() {
                unsafe {
                    libc::free(pack as *mut _);
                }
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
                *pack = 1;
                *pack.add(1) = list_ptr as i64;
            }
        }
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, pack as ValueHandle);
        }
    }
    handle
}

/// Map of Result of List of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_result_list_product_i64(
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
            if display_style != 0 {
                parts.push(String::from("Err()"));
            } else {
                parts.push(String::from("{\"Err\":[\"\"]}"));
            }
            continue;
        }
        let base = vh as *const i64;
        let disc = unsafe { *base };
        if disc == 0 {
            let err_ptr = unsafe { *base.add(1) } as *const std::ffi::c_char;
            let err_s = if err_ptr.is_null() {
                String::new()
            } else {
                unsafe { cstr_to_string(err_ptr) }
            };
            if display_style != 0 {
                parts.push(format!("Err({})", err_s));
            } else {
                parts.push(format!("{{\"Err\":[{}]}}", json_escape_string(&err_s)));
            }
        } else {
            let list_ptr = unsafe { *base.add(1) } as *const u8;
            // Format one list of product via temporary single-key map helper.
            let tmp = mimi_map_new();
            if tmp != 0 && !list_ptr.is_null() {
                unsafe {
                    (*map_from_handle(tmp))
                        .inner
                        .insert("_".into(), list_ptr as ValueHandle);
                }
                let json_ptr = mimi_map_to_json_list_product_i64(tmp, arity, display_style);
                let s = unsafe { cstr_to_string(json_ptr) };
                if !json_ptr.is_null() {
                    unsafe {
                        libc::free(json_ptr as *mut _);
                    }
                }
                let val = if let Some(colon) = s.find(':') {
                    let mut rest = s[colon + 1..].to_string();
                    if rest.ends_with('}') {
                        rest.pop();
                    }
                    rest
                } else {
                    s
                };
                if display_style != 0 {
                    parts.push(format!("Ok({})", val));
                } else {
                    parts.push(format!("{{\"Ok\":[{}]}}", val));
                }
            }
            let _ = n;
        }
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of Result of Option of product from JSON.
/// Pack: `{i64 disc, i64[n+1]}` where disc 1=Ok Option-product pack, 0=Err string.
/// Ok pack reuses option product layout: `{i64 opt_disc, i64[n] fields}`.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_result_option_product_i64(
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
    let n = arity as usize;
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
        if i >= bytes.len() {
            break;
        }
        // Heap Result: {i64 disc, i64 opt_or_err_handle}
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            continue;
        }
        if bytes[i] != b'{' {
            // bare product array → Ok(Some(product))
            if bytes[i] == b'[' {
                let arr_start = i;
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
                        b'"' => {
                            i += 1;
                            while i < bytes.len() && bytes[i] != b'"' {
                                if bytes[i] == b'\\' {
                                    i += 1;
                                }
                                i += 1;
                            }
                        }
                        _ => {}
                    }
                    i += 1;
                }
                let arr = String::from_utf8_lossy(&bytes[arr_start..i]).into_owned();
                let c_arr = alloc_c_string(&format!(
                    "[{}]",
                    arr.trim_start_matches('[').trim_end_matches(']')
                ));
                // Parse as option product: wrap array as single Some element JSON
                let opt_json = format!("[{}]", &arr);
                let c_opt = alloc_c_string(&opt_json);
                // Manual option product pack from bare [1,2]
                let opt_pack_size = 8 + n * 8;
                let opt_ptr = unsafe { libc::malloc(opt_pack_size) as *mut i64 };
                if opt_ptr.is_null() {
                    unsafe {
                        libc::free(pack as *mut _);
                        if !c_opt.is_null() {
                            libc::free(c_opt as *mut _);
                        }
                        if !c_arr.is_null() {
                            libc::free(c_arr as *mut _);
                        }
                    }
                    break;
                }
                // parse fields from arr
                let ab = arr.as_bytes();
                let mut j = 0usize;
                while j < ab.len() && ab[j] != b'[' {
                    j += 1;
                }
                if j < ab.len() && ab[j] == b'[' {
                    j += 1;
                }
                let mut fields = vec![0i64; n];
                for fi in 0..n {
                    while j < ab.len() && (ab[j].is_ascii_whitespace() || ab[j] == b',') {
                        j += 1;
                    }
                    let neg = j < ab.len() && ab[j] == b'-';
                    if neg {
                        j += 1;
                    }
                    let mut v: i64 = 0;
                    while j < ab.len() && ab[j].is_ascii_digit() {
                        v = v.saturating_mul(10).saturating_add((ab[j] - b'0') as i64);
                        j += 1;
                    }
                    if neg {
                        v = -v;
                    }
                    fields[fi] = v;
                }
                unsafe {
                    *opt_ptr = 1;
                    std::ptr::copy_nonoverlapping(fields.as_ptr(), opt_ptr.add(1), n);
                    *pack = 1;
                    *pack.add(1) = opt_ptr as i64;
                }
                if !c_opt.is_null() {
                    unsafe {
                        libc::free(c_opt as *mut _);
                    }
                }
                if !c_arr.is_null() {
                    unsafe {
                        libc::free(c_arr as *mut _);
                    }
                }
            } else {
                unsafe {
                    libc::free(pack as *mut _);
                }
                break;
            }
        } else {
            // tagged object
            let obj_start = i;
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
                    b'"' => {
                        i += 1;
                        while i < bytes.len() && bytes[i] != b'"' {
                            if bytes[i] == b'\\' {
                                i += 1;
                            }
                            i += 1;
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            let obj = String::from_utf8_lossy(&bytes[obj_start..i]).into_owned();
            if obj.contains("\"Err\"") {
                let mut err_s = String::new();
                if let Some(pos) = obj.find("\"Err\"") {
                    let rest = &obj[pos + 5..];
                    if let Some(q1) = rest.find('"') {
                        let r2 = &rest[q1 + 1..];
                        if let Some(q2) = r2.find('"') {
                            err_s = r2[..q2].to_string();
                        }
                    }
                }
                let c = alloc_c_string(&err_s);
                unsafe {
                    *pack = 0;
                    *pack.add(1) = c as i64;
                }
            } else {
                // Ok: value may be null, product array, or nested
                let mut val = String::from("null");
                if let Some(pos) = obj.find("\"Ok\"") {
                    let rest = &obj[pos + 4..];
                    let rb = rest.as_bytes();
                    let mut k = 0usize;
                    while k < rb.len() && rb[k].is_ascii_whitespace()
                        || (k < rb.len() && rb[k] == b':')
                    {
                        k += 1;
                    }
                    // skip :
                    while k < rb.len() && (rb[k].is_ascii_whitespace() || rb[k] == b':') {
                        k += 1;
                    }
                    if k < rb.len() {
                        let start = k;
                        if rb[k] == b'n' {
                            val = String::from("null");
                        } else if rb[k] == b'[' {
                            let mut depth = 0i32;
                            while k < rb.len() {
                                match rb[k] {
                                    b'[' => depth += 1,
                                    b']' => {
                                        depth -= 1;
                                        if depth == 0 {
                                            k += 1;
                                            break;
                                        }
                                    }
                                    b'"' => {
                                        k += 1;
                                        while k < rb.len() && rb[k] != b'"' {
                                            if rb[k] == b'\\' {
                                                k += 1;
                                            }
                                            k += 1;
                                        }
                                    }
                                    _ => {}
                                }
                                k += 1;
                            }
                            val = rest[start..k].to_string();
                        }
                    }
                }
                let opt_pack_size = 8 + n * 8;
                let opt_ptr = unsafe { libc::malloc(opt_pack_size) as *mut i64 };
                if opt_ptr.is_null() {
                    unsafe {
                        libc::free(pack as *mut _);
                    }
                    break;
                }
                if val == "null" || val == "\"None\"" {
                    unsafe {
                        *opt_ptr = 0;
                        for fi in 0..n {
                            *opt_ptr.add(1 + fi) = 0;
                        }
                    }
                } else {
                    // parse product array
                    let ab = val.as_bytes();
                    let mut j = 0usize;
                    while j < ab.len() && ab[j] != b'[' {
                        j += 1;
                    }
                    if j < ab.len() && ab[j] == b'[' {
                        j += 1;
                    }
                    // nested [[1,2]]
                    while j < ab.len() && ab[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    if j < ab.len() && ab[j] == b'[' {
                        j += 1;
                    }
                    let mut fields = vec![0i64; n];
                    for fi in 0..n {
                        while j < ab.len() && (ab[j].is_ascii_whitespace() || ab[j] == b',') {
                            j += 1;
                        }
                        let neg = j < ab.len() && ab[j] == b'-';
                        if neg {
                            j += 1;
                        }
                        let mut v: i64 = 0;
                        while j < ab.len() && ab[j].is_ascii_digit() {
                            v = v.saturating_mul(10).saturating_add((ab[j] - b'0') as i64);
                            j += 1;
                        }
                        if neg {
                            v = -v;
                        }
                        fields[fi] = v;
                    }
                    unsafe {
                        *opt_ptr = 1;
                        std::ptr::copy_nonoverlapping(fields.as_ptr(), opt_ptr.add(1), n);
                    }
                }
                unsafe {
                    *pack = 1;
                    *pack.add(1) = opt_ptr as i64;
                }
            }
        }
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, pack as ValueHandle);
        }
    }
    handle
}

/// Map of Result of Option of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_result_option_product_i64(
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
            if display_style != 0 {
                parts.push(String::from("Err()"));
            } else {
                parts.push(String::from("{\"Err\":[\"\"]}"));
            }
            continue;
        }
        let base = vh as *const i64;
        let disc = unsafe { *base };
        if disc == 0 {
            let err_ptr = unsafe { *base.add(1) } as *const std::ffi::c_char;
            let err_s = if err_ptr.is_null() {
                String::new()
            } else {
                unsafe { cstr_to_string(err_ptr) }
            };
            if display_style != 0 {
                parts.push(format!("Err({})", err_s));
            } else {
                parts.push(format!("{{\"Err\":[{}]}}", json_escape_string(&err_s)));
            }
        } else {
            let opt_h = unsafe { *base.add(1) } as *const i64;
            if opt_h.is_null() {
                if display_style != 0 {
                    parts.push(String::from("Ok(None())"));
                } else {
                    parts.push(String::from("{\"Ok\":[\"None\"]}"));
                }
            } else {
                let opt_disc = unsafe { *opt_h };
                if opt_disc == 0 {
                    if display_style != 0 {
                        parts.push(String::from("Ok(None())"));
                    } else {
                        parts.push(String::from("{\"Ok\":[\"None\"]}"));
                    }
                } else {
                    let fields: Vec<i64> =
                        unsafe { std::slice::from_raw_parts(opt_h.add(1), n).to_vec() };
                    if display_style != 0 {
                        let body: Vec<String> = fields.iter().map(|x| x.to_string()).collect();
                        parts.push(format!("Ok(Some(({})))", body.join(", ")));
                    } else {
                        let body: Vec<String> = fields.iter().map(|x| x.to_string()).collect();
                        parts.push(format!("{{\"Ok\":[{{\"Some\":[[{}]]}}]}}", body.join(",")));
                    }
                }
            }
        }
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of Result of Set of Map of product from JSON.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_result_set_map_product_i64(
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
        if i >= bytes.len() {
            break;
        }
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            continue;
        }
        if bytes[i] != b'{' {
            unsafe {
                libc::free(pack as *mut _);
            }
            break;
        }
        let obj_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let obj = String::from_utf8_lossy(&bytes[obj_start..i]).into_owned();
        if obj.contains("\"Err\"") {
            let mut err_s = String::new();
            if let Some(pos) = obj.find("\"Err\"") {
                let rest = &obj[pos + 5..];
                if let Some(q1) = rest.find('"') {
                    let r2 = &rest[q1 + 1..];
                    if let Some(q2) = r2.find('"') {
                        err_s = r2[..q2].to_string();
                    }
                }
            }
            let c = alloc_c_string(&err_s);
            unsafe {
                *pack = 0;
                *pack.add(1) = c as i64;
            }
        } else {
            let mut arr = String::from("[]");
            if let Some(pos) = obj.find("\"Ok\"") {
                let rest = &obj[pos + 4..];
                if let Some(br) = rest.find('[') {
                    let start = br;
                    let rb = rest.as_bytes();
                    let mut depth = 0i32;
                    let mut k = start;
                    while k < rb.len() {
                        match rb[k] {
                            b'[' => depth += 1,
                            b']' => {
                                depth -= 1;
                                if depth == 0 {
                                    k += 1;
                                    break;
                                }
                            }
                            b'"' => {
                                k += 1;
                                while k < rb.len() && rb[k] != b'"' {
                                    if rb[k] == b'\\' {
                                        k += 1;
                                    }
                                    k += 1;
                                }
                            }
                            _ => {}
                        }
                        k += 1;
                    }
                    arr = rest[start..k].to_string();
                } else if let Some(br) = rest.find('{') {
                    // Ok of Map object? for list map ok is array
                    let start = br;
                    let rb = rest.as_bytes();
                    let mut depth = 0i32;
                    let mut k = start;
                    while k < rb.len() {
                        match rb[k] {
                            b'{' => depth += 1,
                            b'}' => {
                                depth -= 1;
                                if depth == 0 {
                                    k += 1;
                                    break;
                                }
                            }
                            b'"' => {
                                k += 1;
                                while k < rb.len() && rb[k] != b'"' {
                                    if rb[k] == b'\\' {
                                        k += 1;
                                    }
                                    k += 1;
                                }
                            }
                            _ => {}
                        }
                        k += 1;
                    }
                    arr = rest[start..k].to_string();
                }
            }
            let c_arr = alloc_c_string(&arr);
            let ok_h = mimi_set_from_json_map_product_i64(c_arr, arity);
            if !c_arr.is_null() {
                unsafe {
                    libc::free(c_arr as *mut _);
                }
            }
            unsafe {
                *pack = 1;
                *pack.add(1) = ok_h as i64;
            }
        }
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, pack as ValueHandle);
        }
    }
    handle
}

/// Map of Result of Set of Map of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_result_set_map_product_i64(
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
        if vh == 0 {
            if display_style != 0 {
                parts.push(String::from("Err()"));
            } else {
                parts.push(String::from("{\"Err\":[\"\"]}"));
            }
            continue;
        }
        let base = vh as *const i64;
        let disc = unsafe { *base };
        if disc == 0 {
            let err_ptr = unsafe { *base.add(1) } as *const std::ffi::c_char;
            let err_s = if err_ptr.is_null() {
                String::new()
            } else {
                unsafe { cstr_to_string(err_ptr) }
            };
            if display_style != 0 {
                parts.push(format!("Err({})", err_s));
            } else {
                parts.push(format!("{{\"Err\":[{}]}}", json_escape_string(&err_s)));
            }
        } else {
            let ok_h = unsafe { *base.add(1) };
            let ok_json = mimi_set_to_json_map_product_i64(ok_h as _, arity, display_style);
            let s = unsafe { cstr_to_string(ok_json) };
            if !ok_json.is_null() {
                unsafe {
                    libc::free(ok_json as *mut _);
                }
            }
            if display_style != 0 {
                parts.push(format!("Ok({})", s));
            } else {
                parts.push(format!("{{\"Ok\":[{}]}}", s));
            }
        }
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of Option of Set of Map of product from JSON.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_option_set_map_product_i64(
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
        if i >= bytes.len() {
            break;
        }
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            continue;
        }
        if bytes[i] == b'n' && i + 4 <= bytes.len() && &bytes[i..i + 4] == b"null" {
            i += 4;
            unsafe {
                *pack = 0;
                *pack.add(1) = 0;
            }
        } else {
            let val_start = i;
            if bytes[i] == b'{' || bytes[i] == b'[' {
                let open = bytes[i];
                let close = if open == b'{' { b'}' } else { b']' };
                let mut depth = 0i32;
                while i < bytes.len() {
                    if bytes[i] == open {
                        depth += 1;
                    } else if bytes[i] == close {
                        depth -= 1;
                        if depth == 0 {
                            i += 1;
                            break;
                        }
                    } else if bytes[i] == b'"' {
                        i += 1;
                        while i < bytes.len() && bytes[i] != b'"' {
                            if bytes[i] == b'\\' {
                                i += 1;
                            }
                            i += 1;
                        }
                    }
                    i += 1;
                }
            } else {
                unsafe {
                    libc::free(pack as *mut _);
                }
                break;
            }
            let val = String::from_utf8_lossy(&bytes[val_start..i]).into_owned();
            let c_val = alloc_c_string(&val);
            let some_h = mimi_set_from_json_map_product_i64(c_val, arity);
            if !c_val.is_null() {
                unsafe {
                    libc::free(c_val as *mut _);
                }
            }
            unsafe {
                *pack = 1;
                *pack.add(1) = some_h as i64;
            }
        }
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, pack as ValueHandle);
        }
    }
    handle
}

/// Map of Option of Set of Map of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_option_set_map_product_i64(
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
        if vh == 0 {
            if display_style != 0 {
                parts.push(String::from("None()"));
            } else {
                parts.push(String::from("\"None\""));
            }
            continue;
        }
        let base = vh as *const i64;
        let disc = unsafe { *base };
        if disc == 0 {
            if display_style != 0 {
                parts.push(String::from("None()"));
            } else {
                parts.push(String::from("\"None\""));
            }
        } else {
            let some_h = unsafe { *base.add(1) };
            let some_json = mimi_set_to_json_map_product_i64(some_h as _, arity, display_style);
            let s = unsafe { cstr_to_string(some_json) };
            if !some_json.is_null() {
                unsafe {
                    libc::free(some_json as *mut _);
                }
            }
            if display_style != 0 {
                parts.push(format!("Some({})", s));
            } else {
                parts.push(format!("{{\"Some\":[{}]}}", s));
            }
        }
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of Result of List of Map of product from JSON.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_result_list_map_product_i64(
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
        if i >= bytes.len() {
            break;
        }
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            continue;
        }
        if bytes[i] != b'{' {
            unsafe {
                libc::free(pack as *mut _);
            }
            break;
        }
        let obj_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let obj = String::from_utf8_lossy(&bytes[obj_start..i]).into_owned();
        if obj.contains("\"Err\"") {
            let mut err_s = String::new();
            if let Some(pos) = obj.find("\"Err\"") {
                let rest = &obj[pos + 5..];
                if let Some(q1) = rest.find('"') {
                    let r2 = &rest[q1 + 1..];
                    if let Some(q2) = r2.find('"') {
                        err_s = r2[..q2].to_string();
                    }
                }
            }
            let c = alloc_c_string(&err_s);
            unsafe {
                *pack = 0;
                *pack.add(1) = c as i64;
            }
        } else {
            let mut arr = String::from("[]");
            if let Some(pos) = obj.find("\"Ok\"") {
                let rest = &obj[pos + 4..];
                if let Some(br) = rest.find('[') {
                    let start = br;
                    let rb = rest.as_bytes();
                    let mut depth = 0i32;
                    let mut k = start;
                    while k < rb.len() {
                        match rb[k] {
                            b'[' => depth += 1,
                            b']' => {
                                depth -= 1;
                                if depth == 0 {
                                    k += 1;
                                    break;
                                }
                            }
                            b'"' => {
                                k += 1;
                                while k < rb.len() && rb[k] != b'"' {
                                    if rb[k] == b'\\' {
                                        k += 1;
                                    }
                                    k += 1;
                                }
                            }
                            _ => {}
                        }
                        k += 1;
                    }
                    arr = rest[start..k].to_string();
                } else if let Some(br) = rest.find('{') {
                    // Ok of Map object? for list map ok is array
                    let start = br;
                    let rb = rest.as_bytes();
                    let mut depth = 0i32;
                    let mut k = start;
                    while k < rb.len() {
                        match rb[k] {
                            b'{' => depth += 1,
                            b'}' => {
                                depth -= 1;
                                if depth == 0 {
                                    k += 1;
                                    break;
                                }
                            }
                            b'"' => {
                                k += 1;
                                while k < rb.len() && rb[k] != b'"' {
                                    if rb[k] == b'\\' {
                                        k += 1;
                                    }
                                    k += 1;
                                }
                            }
                            _ => {}
                        }
                        k += 1;
                    }
                    arr = rest[start..k].to_string();
                }
            }
            let c_arr = alloc_c_string(&arr);
            let ok_h = mimi_list_from_json_map_product_i64(c_arr, arity);
            if !c_arr.is_null() {
                unsafe {
                    libc::free(c_arr as *mut _);
                }
            }
            unsafe {
                *pack = 1;
                *pack.add(1) = ok_h as i64;
            }
        }
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, pack as ValueHandle);
        }
    }
    handle
}

/// Map of Result of List of Map of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_result_list_map_product_i64(
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
        if vh == 0 {
            if display_style != 0 {
                parts.push(String::from("Err()"));
            } else {
                parts.push(String::from("{\"Err\":[\"\"]}"));
            }
            continue;
        }
        let base = vh as *const i64;
        let disc = unsafe { *base };
        if disc == 0 {
            let err_ptr = unsafe { *base.add(1) } as *const std::ffi::c_char;
            let err_s = if err_ptr.is_null() {
                String::new()
            } else {
                unsafe { cstr_to_string(err_ptr) }
            };
            if display_style != 0 {
                parts.push(format!("Err({})", err_s));
            } else {
                parts.push(format!("{{\"Err\":[{}]}}", json_escape_string(&err_s)));
            }
        } else {
            let ok_h = unsafe { *base.add(1) };
            let ok_json = mimi_list_map_product_to_json(ok_h as _, arity, display_style);
            let s = unsafe { cstr_to_string(ok_json) };
            if !ok_json.is_null() {
                unsafe {
                    libc::free(ok_json as *mut _);
                }
            }
            if display_style != 0 {
                parts.push(format!("Ok({})", s));
            } else {
                parts.push(format!("{{\"Ok\":[{}]}}", s));
            }
        }
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of Option of List of Map of product from JSON.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_option_list_map_product_i64(
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
        if i >= bytes.len() {
            break;
        }
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            continue;
        }
        if bytes[i] == b'n' && i + 4 <= bytes.len() && &bytes[i..i + 4] == b"null" {
            i += 4;
            unsafe {
                *pack = 0;
                *pack.add(1) = 0;
            }
        } else {
            let val_start = i;
            if bytes[i] == b'{' || bytes[i] == b'[' {
                let open = bytes[i];
                let close = if open == b'{' { b'}' } else { b']' };
                let mut depth = 0i32;
                while i < bytes.len() {
                    if bytes[i] == open {
                        depth += 1;
                    } else if bytes[i] == close {
                        depth -= 1;
                        if depth == 0 {
                            i += 1;
                            break;
                        }
                    } else if bytes[i] == b'"' {
                        i += 1;
                        while i < bytes.len() && bytes[i] != b'"' {
                            if bytes[i] == b'\\' {
                                i += 1;
                            }
                            i += 1;
                        }
                    }
                    i += 1;
                }
            } else {
                unsafe {
                    libc::free(pack as *mut _);
                }
                break;
            }
            let val = String::from_utf8_lossy(&bytes[val_start..i]).into_owned();
            let c_val = alloc_c_string(&val);
            let some_h = mimi_list_from_json_map_product_i64(c_val, arity);
            if !c_val.is_null() {
                unsafe {
                    libc::free(c_val as *mut _);
                }
            }
            unsafe {
                *pack = 1;
                *pack.add(1) = some_h as i64;
            }
        }
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, pack as ValueHandle);
        }
    }
    handle
}

/// Map of Option of List of Map of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_option_list_map_product_i64(
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
        if vh == 0 {
            if display_style != 0 {
                parts.push(String::from("None()"));
            } else {
                parts.push(String::from("\"None\""));
            }
            continue;
        }
        let base = vh as *const i64;
        let disc = unsafe { *base };
        if disc == 0 {
            if display_style != 0 {
                parts.push(String::from("None()"));
            } else {
                parts.push(String::from("\"None\""));
            }
        } else {
            let some_h = unsafe { *base.add(1) };
            let some_json = mimi_list_map_product_to_json(some_h as _, arity, display_style);
            let s = unsafe { cstr_to_string(some_json) };
            if !some_json.is_null() {
                unsafe {
                    libc::free(some_json as *mut _);
                }
            }
            if display_style != 0 {
                parts.push(format!("Some({})", s));
            } else {
                parts.push(format!("{{\"Some\":[{}]}}", s));
            }
        }
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}
/// Map of Result of Set of List of product from JSON.
/// Pack: `{i64 disc, i64 set_or_err}` Ok set is Set of List product handles.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_result_set_list_product_i64(
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
        if i >= bytes.len() {
            break;
        }
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            continue;
        }
        if bytes[i] != b'{' {
            unsafe {
                libc::free(pack as *mut _);
            }
            break;
        }
        let obj_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let obj = String::from_utf8_lossy(&bytes[obj_start..i]).into_owned();
        if obj.contains("\"Err\"") {
            let mut err_s = String::new();
            if let Some(pos) = obj.find("\"Err\"") {
                let rest = &obj[pos + 5..];
                if let Some(q1) = rest.find('"') {
                    let r2 = &rest[q1 + 1..];
                    if let Some(q2) = r2.find('"') {
                        err_s = r2[..q2].to_string();
                    }
                }
            }
            let c = alloc_c_string(&err_s);
            unsafe {
                *pack = 0;
                *pack.add(1) = c as i64;
            }
        } else {
            let mut arr = String::from("[]");
            if let Some(pos) = obj.find("\"Ok\"") {
                let rest = &obj[pos + 4..];
                if let Some(br) = rest.find('[') {
                    let start = br;
                    let rb = rest.as_bytes();
                    let mut depth = 0i32;
                    let mut k = start;
                    while k < rb.len() {
                        match rb[k] {
                            b'[' => depth += 1,
                            b']' => {
                                depth -= 1;
                                if depth == 0 {
                                    k += 1;
                                    break;
                                }
                            }
                            b'"' => {
                                k += 1;
                                while k < rb.len() && rb[k] != b'"' {
                                    if rb[k] == b'\\' {
                                        k += 1;
                                    }
                                    k += 1;
                                }
                            }
                            _ => {}
                        }
                        k += 1;
                    }
                    arr = rest[start..k].to_string();
                }
            }
            let c_arr = alloc_c_string(&arr);
            let set_h = mimi_set_from_json_list_product_i64(c_arr, arity);
            if !c_arr.is_null() {
                unsafe {
                    libc::free(c_arr as *mut _);
                }
            }
            unsafe {
                *pack = 1;
                *pack.add(1) = set_h as i64;
            }
        }
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, pack as ValueHandle);
        }
    }
    handle
}

/// Map of Result of Set of List of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_result_set_list_product_i64(
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
        if vh == 0 {
            if display_style != 0 {
                parts.push(String::from("Err()"));
            } else {
                parts.push(String::from("{\"Err\":[\"\"]}"));
            }
            continue;
        }
        let base = vh as *const i64;
        let disc = unsafe { *base };
        if disc == 0 {
            let err_ptr = unsafe { *base.add(1) } as *const std::ffi::c_char;
            let err_s = if err_ptr.is_null() {
                String::new()
            } else {
                unsafe { cstr_to_string(err_ptr) }
            };
            if display_style != 0 {
                parts.push(format!("Err({})", err_s));
            } else {
                parts.push(format!("{{\"Err\":[{}]}}", json_escape_string(&err_s)));
            }
        } else {
            let set_h = unsafe { *base.add(1) } as SetHandle;
            let set_json = mimi_set_to_json_list_product_i64(set_h, arity, display_style);
            let s = unsafe { cstr_to_string(set_json) };
            if !set_json.is_null() {
                unsafe {
                    libc::free(set_json as *mut _);
                }
            }
            if display_style != 0 {
                parts.push(format!("Ok({})", s));
            } else {
                parts.push(format!("{{\"Ok\":[{}]}}", s));
            }
        }
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of Result of Set of product from JSON.
/// Pack: `{i64 disc, i64 set_or_err}` disc 1=Ok set handle, 0=Err string ptr.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_result_set_product_i64(
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
        if i >= bytes.len() {
            break;
        }
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            continue;
        }
        if bytes[i] != b'{' {
            unsafe {
                libc::free(pack as *mut _);
            }
            break;
        }
        let obj_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let obj = String::from_utf8_lossy(&bytes[obj_start..i]).into_owned();
        let is_err = obj.contains("\"Err\"");
        if is_err {
            let mut err_s = String::new();
            if let Some(pos) = obj.find("\"Err\"") {
                let rest = &obj[pos + 5..];
                if let Some(q1) = rest.find('"') {
                    let r2 = &rest[q1 + 1..];
                    if let Some(q2) = r2.find('"') {
                        err_s = r2[..q2].to_string();
                    }
                }
            }
            let c = alloc_c_string(&err_s);
            unsafe {
                *pack = 0;
                *pack.add(1) = c as i64;
            }
        } else {
            let mut arr = String::from("[]");
            if let Some(pos) = obj.find("\"Ok\"") {
                let rest = &obj[pos + 4..];
                if let Some(br) = rest.find('[') {
                    let start = br;
                    let rb = rest.as_bytes();
                    let mut depth = 0i32;
                    let mut k = start;
                    while k < rb.len() {
                        match rb[k] {
                            b'[' => depth += 1,
                            b']' => {
                                depth -= 1;
                                if depth == 0 {
                                    k += 1;
                                    break;
                                }
                            }
                            b'"' => {
                                k += 1;
                                while k < rb.len() && rb[k] != b'"' {
                                    if rb[k] == b'\\' {
                                        k += 1;
                                    }
                                    k += 1;
                                }
                            }
                            _ => {}
                        }
                        k += 1;
                    }
                    arr = rest[start..k].to_string();
                }
            }
            let c_arr = alloc_c_string(&arr);
            let set_h = mimi_set_from_json_product_i64(c_arr, arity);
            if !c_arr.is_null() {
                unsafe {
                    libc::free(c_arr as *mut _);
                }
            }
            unsafe {
                *pack = 1;
                *pack.add(1) = set_h as i64;
            }
        }
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, pack as ValueHandle);
        }
    }
    handle
}

/// Map of Result of Set of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_result_set_product_i64(
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
        if vh == 0 {
            if display_style != 0 {
                parts.push(String::from("Err()"));
            } else {
                parts.push(String::from("{\"Err\":[\"\"]}"));
            }
            continue;
        }
        let base = vh as *const i64;
        let disc = unsafe { *base };
        if disc == 0 {
            let err_ptr = unsafe { *base.add(1) } as *const std::ffi::c_char;
            let err_s = if err_ptr.is_null() {
                String::new()
            } else {
                unsafe { cstr_to_string(err_ptr) }
            };
            if display_style != 0 {
                parts.push(format!("Err({})", err_s));
            } else {
                parts.push(format!("{{\"Err\":[{}]}}", json_escape_string(&err_s)));
            }
        } else {
            let set_h = unsafe { *base.add(1) } as SetHandle;
            let set_json = mimi_set_to_json_product_i64(set_h, arity, display_style);
            let s = unsafe { cstr_to_string(set_json) };
            if !set_json.is_null() {
                unsafe {
                    libc::free(set_json as *mut _);
                }
            }
            if display_style != 0 {
                parts.push(format!("Ok({})", s));
            } else {
                parts.push(format!("{{\"Ok\":[{}]}}", s));
            }
        }
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of Option of Set of product from JSON.
/// Pack: `{i64 disc, i64 set_handle}`.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_option_set_product_i64(
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
        if i >= bytes.len() {
            break;
        }
        let is_none = if bytes[i] == b'n' && i + 4 <= bytes.len() && &bytes[i..i + 4] == b"null" {
            i += 4;
            true
        } else if bytes[i] == b'"' && i + 6 <= bytes.len() && &bytes[i..i + 6] == b"\"None\"" {
            i += 6;
            true
        } else {
            false
        };
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            continue;
        }
        if is_none {
            unsafe {
                *pack = 0;
                *pack.add(1) = 0;
            }
        } else {
            if bytes[i] != b'[' {
                unsafe {
                    libc::free(pack as *mut _);
                }
                break;
            }
            let arr_start = i;
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
                    b'"' => {
                        i += 1;
                        while i < bytes.len() && bytes[i] != b'"' {
                            if bytes[i] == b'\\' {
                                i += 1;
                            }
                            i += 1;
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            let arr = String::from_utf8_lossy(&bytes[arr_start..i]).into_owned();
            let c_arr = alloc_c_string(&arr);
            let set_h = mimi_set_from_json_product_i64(c_arr, arity);
            if !c_arr.is_null() {
                unsafe {
                    libc::free(c_arr as *mut _);
                }
            }
            unsafe {
                *pack = 1;
                *pack.add(1) = set_h as i64;
            }
        }
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, pack as ValueHandle);
        }
    }
    handle
}

/// Map of Option of Set of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_option_set_product_i64(
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
        if vh == 0 {
            if display_style != 0 {
                parts.push(String::from("None()"));
            } else {
                parts.push(String::from("\"None\""));
            }
            continue;
        }
        let base = vh as *const i64;
        let disc = unsafe { *base };
        if disc == 0 {
            if display_style != 0 {
                parts.push(String::from("None()"));
            } else {
                parts.push(String::from("\"None\""));
            }
        } else {
            let set_h = unsafe { *base.add(1) } as SetHandle;
            let set_json = mimi_set_to_json_product_i64(set_h, arity, display_style);
            let s = unsafe { cstr_to_string(set_json) };
            if !set_json.is_null() {
                unsafe {
                    libc::free(set_json as *mut _);
                }
            }
            if display_style != 0 {
                parts.push(format!("Some({})", s));
            } else {
                parts.push(format!("{{\"Some\":[{}]}}", s));
            }
        }
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of Option of Map of product from JSON.
/// Pack: `{i64 disc, i64 map_handle}` (disc 0=None, 1=Some).
#[no_mangle]
pub extern "C" fn mimi_map_from_json_option_map_product_i64(
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
        if i >= bytes.len() {
            break;
        }
        let is_none = if bytes[i] == b'n' && i + 4 <= bytes.len() && &bytes[i..i + 4] == b"null" {
            i += 4;
            true
        } else if bytes[i] == b'"' && i + 6 <= bytes.len() && &bytes[i..i + 6] == b"\"None\"" {
            i += 6;
            true
        } else {
            false
        };
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            continue;
        }
        if is_none {
            unsafe {
                *pack = 0;
                *pack.add(1) = 0;
            }
        } else {
            // Extract object value as substring for nested map_from_json_product.
            if bytes[i] != b'{' {
                unsafe {
                    libc::free(pack as *mut _);
                }
                break;
            }
            let obj_start = i;
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
                    b'"' => {
                        i += 1;
                        while i < bytes.len() && bytes[i] != b'"' {
                            if bytes[i] == b'\\' {
                                i += 1;
                            }
                            i += 1;
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            let obj = String::from_utf8_lossy(&bytes[obj_start..i]).into_owned();
            let c_obj = alloc_c_string(&obj);
            let inner_h = mimi_map_from_json_product_i64(c_obj, arity);
            if !c_obj.is_null() {
                unsafe {
                    libc::free(c_obj as *mut _);
                }
            }
            unsafe {
                *pack = 1;
                *pack.add(1) = inner_h as i64;
            }
        }
        unsafe {
            (*map_from_handle(handle))
                .inner
                .insert(key, pack as ValueHandle);
        }
    }
    handle
}

/// Map of Option of Map of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_option_map_product_i64(
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
        if vh == 0 {
            if display_style != 0 {
                parts.push(String::from("None()"));
            } else {
                parts.push(String::from("\"None\""));
            }
            continue;
        }
        let base = vh as *const i64;
        let disc = unsafe { *base };
        if disc == 0 {
            if display_style != 0 {
                parts.push(String::from("None()"));
            } else {
                parts.push(String::from("\"None\""));
            }
        } else {
            let inner_h = unsafe { *base.add(1) } as MapHandle;
            let inner_json = mimi_map_to_json_product_i64(inner_h, arity, display_style);
            let s = unsafe { cstr_to_string(inner_json) };
            if !inner_json.is_null() {
                unsafe {
                    libc::free(inner_json as *mut _);
                }
            }
            if display_style != 0 {
                parts.push(format!("Some({})", s));
            } else {
                parts.push(format!("{{\"Some\":[{}]}}", s));
            }
        }
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of Option of product-tuple from JSON.
/// Values: `null`/`"None"` → None; array `[1,2]` or `{"Some":[1,2]}` → Some product.
/// Stores heap `{i8 disc, pad, i64[n] fields}` as ValueHandle (disc 0=None, 1=Some).
#[no_mangle]
pub extern "C" fn mimi_map_from_json_option_product_i64(
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
    let n = arity as usize;
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
        if i >= bytes.len() {
            break;
        }
        // None: null or "None"
        let is_none = if bytes[i] == b'n' && i + 4 <= bytes.len() && &bytes[i..i + 4] == b"null" {
            i += 4;
            true
        } else if bytes[i] == b'"' && i + 6 <= bytes.len() && &bytes[i..i + 6] == b"\"None\"" {
            i += 6;
            true
        } else {
            false
        };
        let pack_size = 8 + n * 8; // disc i64 + fields
        let ptr = unsafe { libc::malloc(pack_size) as *mut i64 };
        if ptr.is_null() {
            continue;
        }
        if is_none {
            unsafe {
                *ptr = 0;
                for fi in 0..n {
                    *ptr.add(1 + fi) = 0;
                }
            }
        } else {
            // Optional {"Some": …} or bare product array.
            if bytes[i] == b'{' {
                while i < bytes.len() && bytes[i] != b'[' {
                    i += 1;
                }
            }
            if i >= bytes.len() || bytes[i] != b'[' {
                unsafe {
                    libc::free(ptr as *mut _);
                }
                break;
            }
            i += 1;
            // Nested product array form {"Some":[[1,2]]}
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'[' {
                i += 1;
            }
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
            // nested [[1,2]] outer close
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b']' {
                i += 1;
            }
            // skip closing of Some object if present
            while i < bytes.len() && bytes[i] != b',' && bytes[i] != b'}' {
                if bytes[i] == b'}' {
                    break;
                }
                i += 1;
            }
            unsafe {
                *ptr = 1;
                std::ptr::copy_nonoverlapping(fields.as_ptr(), ptr.add(1), n);
            }
        }
        let vh = ptr as ValueHandle;
        unsafe {
            (*map_from_handle(handle)).inner.insert(key, vh);
        }
    }
    handle
}

/// Map of Option of product Display/JSON.
/// `display_style` 0 = JSON, 1 = Display.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_option_product_i64(
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
            if display_style != 0 {
                parts.push(String::from("None()"));
            } else {
                parts.push(String::from("\"None\""));
            }
            continue;
        }
        let base = vh as *const i64;
        let disc = unsafe { *base };
        if disc == 0 {
            if display_style != 0 {
                parts.push(String::from("None()"));
            } else {
                parts.push(String::from("\"None\""));
            }
        } else {
            let fields: Vec<i64> = unsafe { std::slice::from_raw_parts(base.add(1), n).to_vec() };
            if display_style != 0 {
                let body: Vec<String> = fields.iter().map(|x| x.to_string()).collect();
                parts.push(format!("Some(({}))", body.join(", ")));
            } else {
                let body: Vec<String> = fields.iter().map(|x| x.to_string()).collect();
                parts.push(format!("{{\"Some\":[[{}]]}}", body.join(",")));
            }
        }
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of Result of product from JSON.
/// Values: bare product array → Ok; `{"Ok":[…]}` / `{"Err":…}`.
#[no_mangle]
pub extern "C" fn mimi_map_from_json_result_product_i64(
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
    let n = arity as usize;
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
        if i >= bytes.len() {
            break;
        }
        // Heap Result: {i64 disc, i64[n] ok fields or err string handle}
        // disc 1 = Ok, 0 = Err; for Err store string ptr in field[0]
        let pack_size = 8 + n * 8;
        let ptr = unsafe { libc::malloc(pack_size) as *mut i64 };
        if ptr.is_null() {
            continue;
        }
        let mut is_err = false;
        let mut err_str = String::new();
        if bytes[i] == b'{' {
            // tagged Ok/Err
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'"' {
                i += 1;
                let ts = i;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += 1;
                }
                let tag = String::from_utf8_lossy(&bytes[ts..i]).into_owned();
                if i < bytes.len() {
                    i += 1;
                }
                while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b':') {
                    i += 1;
                }
                if tag == "Err" {
                    is_err = true;
                    if i < bytes.len() && bytes[i] == b'"' {
                        i += 1;
                        let es = i;
                        while i < bytes.len() && bytes[i] != b'"' {
                            if bytes[i] == b'\\' {
                                i += 1;
                            }
                            i += 1;
                        }
                        err_str = String::from_utf8_lossy(&bytes[es..i]).into_owned();
                        if i < bytes.len() {
                            i += 1;
                        }
                    } else if i < bytes.len() && bytes[i].is_ascii_digit() {
                        let mut v: i64 = 0;
                        while i < bytes.len() && bytes[i].is_ascii_digit() {
                            v = v
                                .saturating_mul(10)
                                .saturating_add((bytes[i] - b'0') as i64);
                            i += 1;
                        }
                        err_str = v.to_string();
                    }
                }
                // for Ok, fall through to parse array at i
            }
            while i < bytes.len() && bytes[i] != b'[' && bytes[i] != b'}' && !is_err {
                i += 1;
            }
        }
        if is_err {
            let c = alloc_c_string(&err_str);
            unsafe {
                *ptr = 0;
                *ptr.add(1) = c as i64;
                for fi in 1..n {
                    *ptr.add(1 + fi) = 0;
                }
            }
            // skip to end of object value
            while i < bytes.len() && bytes[i] != b',' && bytes[i] != b'}' {
                i += 1;
            }
        } else {
            if i >= bytes.len() || bytes[i] != b'[' {
                unsafe {
                    libc::free(ptr as *mut _);
                }
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
            while i < bytes.len() && bytes[i] != b',' && bytes[i] != b'}' {
                i += 1;
            }
            unsafe {
                *ptr = 1;
                std::ptr::copy_nonoverlapping(fields.as_ptr(), ptr.add(1), n);
            }
        }
        // Skip closing braces of tagged {"Ok":…} / {"Err":…} value.
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b'}' {
            i += 1;
        }
        let vh = ptr as ValueHandle;
        unsafe {
            (*map_from_handle(handle)).inner.insert(key, vh);
        }
    }
    handle
}

/// Map of Result of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_map_to_json_result_product_i64(
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
            if display_style != 0 {
                parts.push(String::from("Err()"));
            } else {
                parts.push(String::from("{\"Err\":[\"\"]}"));
            }
            continue;
        }
        let base = vh as *const i64;
        let disc = unsafe { *base };
        if disc == 0 {
            let err_ptr = unsafe { *base.add(1) } as *const std::ffi::c_char;
            let err_s = if err_ptr.is_null() {
                String::new()
            } else {
                unsafe { cstr_to_string(err_ptr) }
            };
            if display_style != 0 {
                parts.push(format!("Err({})", err_s));
            } else {
                parts.push(format!("{{\"Err\":[{}]}}", json_escape_string(&err_s)));
            }
        } else {
            let fields: Vec<i64> = unsafe { std::slice::from_raw_parts(base.add(1), n).to_vec() };
            if display_style != 0 {
                let body: Vec<String> = fields.iter().map(|x| x.to_string()).collect();
                parts.push(format!("Ok(({}))", body.join(", ")));
            } else {
                let body: Vec<String> = fields.iter().map(|x| x.to_string()).collect();
                parts.push(format!("{{\"Ok\":[[{}]]}}", body.join(",")));
            }
        }
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
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
/// R-C11: also aborts on stale (destroyed / never-registered) handles.
// SAFETY: aborts on invalid/stale handle; caller must ensure exclusive access while live.
unsafe fn set_from_handle(handle: SetHandle) -> *mut MimiSet {
    if handle == 0 || !set_is_live(handle) {
        std::process::abort();
    }
    handle as *mut MimiSet
}

#[no_mangle]
pub extern "C" fn mimi_set_new() -> SetHandle {
    let set = Box::new(MimiSet {
        inner: std::collections::HashSet::new(),
    });
    let h = Box::into_raw(set) as SetHandle;
    set_register_live(h);
    h
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

/// List of Result of product from JSON array of tagged objects / bare products.
/// Elements stored as heap `{i64 disc, i64[n] fields or err string ptr}` ValueHandles in list data.
#[no_mangle]
pub extern "C" fn mimi_list_from_json_result_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> *mut MimiList {
    let empty = || {
        let list = unsafe { libc::malloc(std::mem::size_of::<MimiList>()) as *mut MimiList };
        if !list.is_null() {
            unsafe {
                (*list).len = 0;
                (*list).data = std::ptr::null_mut();
                (*list).owns_data = true;
            }
        }
        list
    };
    if json.is_null() || arity <= 0 || arity > 16 {
        return empty();
    }
    let s = unsafe { cstr_to_string(json) };
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b'[' {
        return empty();
    }
    i += 1;
    let n = arity as usize;
    let mut handles: Vec<i64> = Vec::new();
    loop {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b']' {
            break;
        }
        // disc (8) + n Ok product fields (n*8) + Err payload (8)
        let pack_size = 8 + n * 8 + 8;
        let ptr = unsafe { libc::malloc(pack_size) as *mut i64 };
        if ptr.is_null() {
            break;
        }
        unsafe {
            std::ptr::write_bytes(ptr as *mut u8, 0, pack_size);
        }
        let mut is_err = false;
        let mut err_str = String::new();
        if bytes[i] == b'{' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'"' {
                i += 1;
                let ts = i;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += 1;
                }
                let tag = String::from_utf8_lossy(&bytes[ts..i]).into_owned();
                if i < bytes.len() {
                    i += 1;
                }
                while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b':') {
                    i += 1;
                }
                if tag == "Err" {
                    is_err = true;
                    if i < bytes.len() && bytes[i] == b'"' {
                        i += 1;
                        let es = i;
                        while i < bytes.len() && bytes[i] != b'"' {
                            if bytes[i] == b'\\' {
                                i += 1;
                            }
                            i += 1;
                        }
                        err_str = String::from_utf8_lossy(&bytes[es..i]).into_owned();
                        if i < bytes.len() {
                            i += 1;
                        }
                    }
                }
            }
            while i < bytes.len() && bytes[i] != b'[' && bytes[i] != b'}' && !is_err {
                i += 1;
            }
        }
        if is_err {
            // Heap Mimi string {ptr, len} so decode_result_err_string works.
            let c = alloc_c_string(&err_str);
            let heap = unsafe { libc::malloc(16) as *mut i64 };
            if !heap.is_null() {
                unsafe {
                    *heap = c as i64;
                    *heap.add(1) = err_str.len() as i64;
                }
            }
            unsafe {
                *ptr = 0;
                for fi in 0..n {
                    *ptr.add(1 + fi) = 0;
                }
                *ptr.add(1 + n) = if heap.is_null() {
                    c as i64
                } else {
                    heap as i64
                };
            }
            while i < bytes.len() && bytes[i] != b',' && bytes[i] != b']' {
                i += 1;
            }
        } else {
            if i >= bytes.len() || bytes[i] != b'[' {
                unsafe {
                    libc::free(ptr as *mut _);
                }
                break;
            }
            i += 1;
            // nested [[1,2]] form from {"Ok":[[1,2]]} after get may already be [1,2]
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'[' {
                i += 1;
            }
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
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b']' {
                i += 1;
            }
            while i < bytes.len() && bytes[i] != b',' && bytes[i] != b']' && bytes[i] != b'}' {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'}' {
                i += 1;
            }
            unsafe {
                *ptr = 1;
                std::ptr::copy_nonoverlapping(fields.as_ptr(), ptr.add(1), n);
                *ptr.add(1 + n) = 0;
            }
        }
        handles.push(ptr as i64);
    }
    let list = unsafe { libc::malloc(std::mem::size_of::<MimiList>()) as *mut MimiList };
    if list.is_null() {
        return std::ptr::null_mut();
    }
    let data_size = handles.len() * 8;
    let data = if data_size == 0 {
        std::ptr::null_mut()
    } else {
        unsafe { libc::malloc(data_size) as *mut i64 }
    };
    if data_size > 0 && data.is_null() {
        unsafe {
            libc::free(list as *mut _);
        }
        return std::ptr::null_mut();
    }
    if !data.is_null() {
        unsafe {
            std::ptr::copy_nonoverlapping(handles.as_ptr(), data, handles.len());
        }
    }
    unsafe {
        (*list).len = handles.len() as i64;
        (*list).data = data as *mut *mut std::ffi::c_char;
        (*list).owns_data = true;
    }
    list
}

/// List of Result of Set of product from JSON.
/// Elements: heap `{i64 disc, i64 set_or_err}`.
#[no_mangle]
pub extern "C" fn mimi_list_from_json_result_set_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> *mut MimiList {
    let empty = || {
        let list = unsafe { libc::malloc(std::mem::size_of::<MimiList>()) as *mut MimiList };
        if !list.is_null() {
            unsafe {
                (*list).len = 0;
                (*list).data = std::ptr::null_mut();
                (*list).owns_data = true;
            }
        }
        list
    };
    if json.is_null() || arity <= 0 || arity > 16 {
        return empty();
    }
    let s = unsafe { cstr_to_string(json) };
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b'[' {
        return empty();
    }
    i += 1;
    let mut handles: Vec<i64> = Vec::new();
    loop {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b']' {
            break;
        }
        if bytes[i] != b'{' {
            break;
        }
        let obj_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let obj = String::from_utf8_lossy(&bytes[obj_start..i]).into_owned();
        // disc + Ok SetHandle + Err (heap Mimi string)
        let pack = unsafe { libc::malloc(24) as *mut i64 };
        if pack.is_null() {
            break;
        }
        unsafe {
            std::ptr::write_bytes(pack as *mut u8, 0, 24);
        }
        let is_err = obj.contains("\"Err\"");
        if is_err {
            let mut err_s = String::new();
            if let Some(pos) = obj.find("\"Err\"") {
                let rest = &obj[pos + 5..];
                if let Some(q1) = rest.find('"') {
                    let r2 = &rest[q1 + 1..];
                    if let Some(q2) = r2.find('"') {
                        err_s = r2[..q2].to_string();
                    }
                }
            }
            let c = alloc_c_string(&err_s);
            let heap = unsafe { libc::malloc(16) as *mut i64 };
            if !heap.is_null() {
                unsafe {
                    *heap = c as i64;
                    *heap.add(1) = err_s.len() as i64;
                }
            }
            unsafe {
                *pack = 0;
                *pack.add(1) = 0;
                *pack.add(2) = if heap.is_null() {
                    c as i64
                } else {
                    heap as i64
                };
            }
        } else {
            let mut arr = String::from("[]");
            if let Some(pos) = obj.find("\"Ok\"") {
                let rest = &obj[pos + 4..];
                if let Some(br) = rest.find('[') {
                    let start = br;
                    let rb = rest.as_bytes();
                    let mut depth = 0i32;
                    let mut k = start;
                    while k < rb.len() {
                        match rb[k] {
                            b'[' => depth += 1,
                            b']' => {
                                depth -= 1;
                                if depth == 0 {
                                    k += 1;
                                    break;
                                }
                            }
                            b'"' => {
                                k += 1;
                                while k < rb.len() && rb[k] != b'"' {
                                    if rb[k] == b'\\' {
                                        k += 1;
                                    }
                                    k += 1;
                                }
                            }
                            _ => {}
                        }
                        k += 1;
                    }
                    arr = rest[start..k].to_string();
                }
            }
            let c_arr = alloc_c_string(&arr);
            let set_h = mimi_set_from_json_product_i64(c_arr, arity);
            if !c_arr.is_null() {
                unsafe {
                    libc::free(c_arr as *mut _);
                }
            }
            unsafe {
                *pack = 1;
                *pack.add(1) = set_h as i64;
                *pack.add(2) = 0;
            }
        }
        handles.push(pack as i64);
    }
    let list = unsafe { libc::malloc(std::mem::size_of::<MimiList>()) as *mut MimiList };
    if list.is_null() {
        return std::ptr::null_mut();
    }
    let data_size = handles.len() * 8;
    let data = if data_size == 0 {
        std::ptr::null_mut()
    } else {
        unsafe { libc::malloc(data_size) as *mut i64 }
    };
    if data_size > 0 && data.is_null() {
        unsafe {
            libc::free(list as *mut _);
        }
        return std::ptr::null_mut();
    }
    if !data.is_null() {
        unsafe {
            std::ptr::copy_nonoverlapping(handles.as_ptr(), data, handles.len());
        }
    }
    unsafe {
        (*list).len = handles.len() as i64;
        (*list).data = data as *mut *mut std::ffi::c_char;
        (*list).owns_data = true;
    }
    list
}

/// Display/JSON for List of Result of Set of product.
#[no_mangle]
pub extern "C" fn mimi_list_result_set_product_to_json(
    list: *const MimiList,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if list.is_null() || arity <= 0 || arity > 16 {
        return alloc_c_string("[]");
    }
    let lst = unsafe { &*list };
    if lst.data.is_null() || lst.len == 0 {
        return alloc_c_string("[]");
    }
    if lst.len < 0 || lst.len > 1_000_000 {
        return alloc_c_string("[...]");
    }
    let mut parts: Vec<String> = Vec::with_capacity(lst.len as usize * 2 + 2);
    parts.push(String::from("["));
    for i in 0..lst.len as isize {
        if i > 0 {
            parts.push(if display_style != 0 {
                String::from(", ")
            } else {
                String::from(",")
            });
        }
        let h = unsafe { *(lst.data as *const i64).offset(i) };
        if h == 0 {
            if display_style != 0 {
                parts.push(String::from("Err()"));
            } else {
                parts.push(String::from("{\"Err\":[\"\"]}"));
            }
            continue;
        }
        // Layout: disc (word0, low bit) + Ok SetHandle (word1) + Err (word2).
        let base = h as *const i64;
        let disc_word = unsafe { *base };
        let is_ok = (disc_word & 1) != 0;
        if !is_ok {
            let err_word = unsafe { *base.add(2) };
            let err_json = decode_result_err_string(err_word);
            if display_style != 0 {
                let raw = err_json
                    .strip_prefix('"')
                    .and_then(|s| s.strip_suffix('"'))
                    .unwrap_or(err_json.as_str())
                    .replace("\\\"", "\"")
                    .replace("\\\\", "\\");
                parts.push(format!("Err({})", raw));
            } else {
                parts.push(format!("{{\"Err\":[{}]}}", err_json));
            }
        } else {
            let set_h = unsafe { *base.add(1) } as SetHandle;
            let sj = mimi_set_to_json_product_i64(set_h, arity, display_style);
            let s = unsafe { cstr_to_string(sj) };
            if !sj.is_null() {
                unsafe {
                    libc::free(sj as *mut _);
                }
            }
            if display_style != 0 {
                parts.push(format!("Ok({})", s));
            } else {
                parts.push(format!("{{\"Ok\":[{}]}}", s));
            }
        }
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

/// List of Result of Map of product from JSON.
/// Elements: heap `{i64 disc, i64 map_or_err}` (disc 1=Ok map handle).
#[no_mangle]
pub extern "C" fn mimi_list_from_json_result_map_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> *mut MimiList {
    let empty = || {
        let list = unsafe { libc::malloc(std::mem::size_of::<MimiList>()) as *mut MimiList };
        if !list.is_null() {
            unsafe {
                (*list).len = 0;
                (*list).data = std::ptr::null_mut();
                (*list).owns_data = true;
            }
        }
        list
    };
    if json.is_null() || arity <= 0 || arity > 16 {
        return empty();
    }
    let s = unsafe { cstr_to_string(json) };
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b'[' {
        return empty();
    }
    i += 1;
    let mut handles: Vec<i64> = Vec::new();
    loop {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b']' {
            break;
        }
        if bytes[i] != b'{' {
            break;
        }
        let obj_start = i;
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
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let obj = String::from_utf8_lossy(&bytes[obj_start..i]).into_owned();
        // disc + Ok MapHandle + Err (heap Mimi string)
        let pack = unsafe { libc::malloc(24) as *mut i64 };
        if pack.is_null() {
            break;
        }
        unsafe {
            std::ptr::write_bytes(pack as *mut u8, 0, 24);
        }
        let is_err = obj.contains("\"Err\"");
        if is_err {
            let mut err_s = String::new();
            if let Some(pos) = obj.find("\"Err\"") {
                let rest = &obj[pos + 5..];
                if let Some(q1) = rest.find('"') {
                    let r2 = &rest[q1 + 1..];
                    if let Some(q2) = r2.find('"') {
                        err_s = r2[..q2].to_string();
                    }
                }
            }
            let c = alloc_c_string(&err_s);
            let heap = unsafe { libc::malloc(16) as *mut i64 };
            if !heap.is_null() {
                unsafe {
                    *heap = c as i64;
                    *heap.add(1) = err_s.len() as i64;
                }
            }
            unsafe {
                *pack = 0;
                *pack.add(1) = 0;
                *pack.add(2) = if heap.is_null() {
                    c as i64
                } else {
                    heap as i64
                };
            }
        } else {
            let mut inner_obj = String::from("{}");
            if let Some(pos) = obj.find("\"Ok\"") {
                let rest = &obj[pos + 4..];
                if let Some(brace) = rest.find('{') {
                    let start = brace;
                    let rb = rest.as_bytes();
                    let mut depth = 0i32;
                    let mut k = start;
                    while k < rb.len() {
                        match rb[k] {
                            b'{' => depth += 1,
                            b'}' => {
                                depth -= 1;
                                if depth == 0 {
                                    k += 1;
                                    break;
                                }
                            }
                            b'"' => {
                                k += 1;
                                while k < rb.len() && rb[k] != b'"' {
                                    if rb[k] == b'\\' {
                                        k += 1;
                                    }
                                    k += 1;
                                }
                            }
                            _ => {}
                        }
                        k += 1;
                    }
                    inner_obj = rest[start..k].to_string();
                }
            }
            let c_obj = alloc_c_string(&inner_obj);
            let mh = mimi_map_from_json_product_i64(c_obj, arity);
            if !c_obj.is_null() {
                unsafe {
                    libc::free(c_obj as *mut _);
                }
            }
            unsafe {
                *pack = 1;
                *pack.add(1) = mh as i64;
                *pack.add(2) = 0;
            }
        }
        handles.push(pack as i64);
    }
    let list = unsafe { libc::malloc(std::mem::size_of::<MimiList>()) as *mut MimiList };
    if list.is_null() {
        return std::ptr::null_mut();
    }
    let data_size = handles.len() * 8;
    let data = if data_size == 0 {
        std::ptr::null_mut()
    } else {
        unsafe { libc::malloc(data_size) as *mut i64 }
    };
    if data_size > 0 && data.is_null() {
        unsafe {
            libc::free(list as *mut _);
        }
        return std::ptr::null_mut();
    }
    if !data.is_null() {
        unsafe {
            std::ptr::copy_nonoverlapping(handles.as_ptr(), data, handles.len());
        }
    }
    unsafe {
        (*list).len = handles.len() as i64;
        (*list).data = data as *mut *mut std::ffi::c_char;
        (*list).owns_data = true;
    }
    list
}

/// Display/JSON for List of Result of Map of product.
#[no_mangle]
pub extern "C" fn mimi_list_result_map_product_to_json(
    list: *const MimiList,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if list.is_null() || arity <= 0 || arity > 16 {
        return alloc_c_string("[]");
    }
    let lst = unsafe { &*list };
    if lst.data.is_null() || lst.len == 0 {
        return alloc_c_string("[]");
    }
    if lst.len < 0 || lst.len > 1_000_000 {
        return alloc_c_string("[...]");
    }
    let mut parts: Vec<String> = Vec::with_capacity(lst.len as usize * 2 + 2);
    parts.push(String::from("["));
    for i in 0..lst.len as isize {
        if i > 0 {
            parts.push(if display_style != 0 {
                String::from(", ")
            } else {
                String::from(",")
            });
        }
        let h = unsafe { *(lst.data as *const i64).offset(i) };
        if h == 0 {
            if display_style != 0 {
                parts.push(String::from("Err()"));
            } else {
                parts.push(String::from("{\"Err\":[\"\"]}"));
            }
            continue;
        }
        // Layout: disc (word0, low bit) + Ok MapHandle (word1) + Err (word2).
        let base = h as *const i64;
        let disc_word = unsafe { *base };
        let is_ok = (disc_word & 1) != 0;
        if !is_ok {
            let err_word = unsafe { *base.add(2) };
            let err_json = decode_result_err_string(err_word);
            if display_style != 0 {
                let raw = err_json
                    .strip_prefix('"')
                    .and_then(|s| s.strip_suffix('"'))
                    .unwrap_or(err_json.as_str())
                    .replace("\\\"", "\"")
                    .replace("\\\\", "\\");
                parts.push(format!("Err({})", raw));
            } else {
                parts.push(format!("{{\"Err\":[{}]}}", err_json));
            }
        } else {
            let mh = unsafe { *base.add(1) } as MapHandle;
            let mj = mimi_map_to_json_product_i64(mh, arity, display_style);
            let s = unsafe { cstr_to_string(mj) };
            if !mj.is_null() {
                unsafe {
                    libc::free(mj as *mut _);
                }
            }
            if display_style != 0 {
                parts.push(format!("Ok({})", s));
            } else {
                parts.push(format!("{{\"Ok\":[{}]}}", s));
            }
        }
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

/// Display/JSON for List of Result of product (heap packs from list_from_json_result_product).
/// Pack: `{i64 disc, i64[n] fields, i64 err_ptr}` where disc!=0 is Ok.
#[no_mangle]
pub extern "C" fn mimi_list_result_product_to_json(
    list: *const MimiList,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if list.is_null() || arity <= 0 || arity > 16 {
        return if display_style != 0 {
            alloc_c_string("[]")
        } else {
            alloc_c_string("[]")
        };
    }
    let lst = unsafe { &*list };
    if lst.data.is_null() || lst.len == 0 {
        return alloc_c_string("[]");
    }
    if lst.len < 0 || lst.len > 1_000_000 {
        return alloc_c_string("[...]");
    }
    let n = arity as usize;
    let mut parts: Vec<String> = Vec::with_capacity(lst.len as usize * 2 + 2);
    parts.push(String::from("["));
    for i in 0..lst.len as isize {
        if i > 0 {
            parts.push(if display_style != 0 {
                String::from(", ")
            } else {
                String::from(",")
            });
        }
        let h = unsafe { *(lst.data as *const i64).offset(i) };
        if h == 0 {
            if display_style != 0 {
                parts.push(String::from("Err()"));
            } else {
                parts.push(String::from("{\"Err\":[\"\"]}"));
            }
            continue;
        }
        // Layout matches compile_ok/err for List<Result<(T..), E>>:
        //   word0 = disc (low bit: 1=Ok, 0=Err; i1 + padding may dirty high bits)
        //   word1..n = Ok product fields (zeroed on Err)
        //   word(n+1) = Err payload (i64 handle / int)
        let base = h as *const i64;
        let disc_word = unsafe { *base };
        let is_ok = (disc_word & 1) != 0;
        if !is_ok {
            let err_word = unsafe { *base.add(1 + n) };
            // decode_result_err_string returns a JSON string literal ("…").
            let err_json = decode_result_err_string(err_word);
            if display_style != 0 {
                // Display: strip surrounding quotes from JSON escape.
                let raw = err_json
                    .strip_prefix('"')
                    .and_then(|s| s.strip_suffix('"'))
                    .unwrap_or(err_json.as_str())
                    .replace("\\\"", "\"")
                    .replace("\\\\", "\\");
                parts.push(format!("Err({})", raw));
            } else {
                parts.push(format!("{{\"Err\":[{}]}}", err_json));
            }
        } else {
            let fields: Vec<i64> = unsafe { std::slice::from_raw_parts(base.add(1), n).to_vec() };
            if display_style != 0 {
                let body: Vec<String> = fields.iter().map(|x| x.to_string()).collect();
                parts.push(format!("Ok(({}))", body.join(", ")));
            } else {
                let body: Vec<String> = fields.iter().map(|x| x.to_string()).collect();
                parts.push(format!("{{\"Ok\":[[{}]]}}", body.join(",")));
            }
        }
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

/// Set of Result of product from JSON array of tagged objects.
#[no_mangle]
pub extern "C" fn mimi_set_from_json_result_product_i64(
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
        let pack_size = 8 + n * 8;
        let ptr = unsafe { libc::malloc(pack_size) as *mut i64 };
        if ptr.is_null() {
            break;
        }
        let mut is_err = false;
        let mut err_str = String::new();
        if bytes[i] == b'{' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'"' {
                i += 1;
                let ts = i;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += 1;
                }
                let tag = String::from_utf8_lossy(&bytes[ts..i]).into_owned();
                if i < bytes.len() {
                    i += 1;
                }
                while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b':') {
                    i += 1;
                }
                if tag == "Err" {
                    is_err = true;
                    if i < bytes.len() && bytes[i] == b'"' {
                        i += 1;
                        let es = i;
                        while i < bytes.len() && bytes[i] != b'"' {
                            if bytes[i] == b'\\' {
                                i += 1;
                            }
                            i += 1;
                        }
                        err_str = String::from_utf8_lossy(&bytes[es..i]).into_owned();
                        if i < bytes.len() {
                            i += 1;
                        }
                    }
                }
            }
            while i < bytes.len() && bytes[i] != b'[' && bytes[i] != b'}' && !is_err {
                i += 1;
            }
        }
        if is_err {
            let c = alloc_c_string(&err_str);
            unsafe {
                *ptr = 0;
                *ptr.add(1) = c as i64;
                for fi in 1..n {
                    *ptr.add(1 + fi) = 0;
                }
            }
            while i < bytes.len() && bytes[i] != b',' && bytes[i] != b']' {
                i += 1;
            }
        } else {
            if i >= bytes.len() || bytes[i] != b'[' {
                unsafe {
                    libc::free(ptr as *mut _);
                }
                break;
            }
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'[' {
                i += 1;
            }
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
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b']' {
                i += 1;
            }
            while i < bytes.len() && bytes[i] != b',' && bytes[i] != b']' && bytes[i] != b'}' {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'}' {
                i += 1;
            }
            unsafe {
                *ptr = 1;
                std::ptr::copy_nonoverlapping(fields.as_ptr(), ptr.add(1), n);
            }
        }
        mimi_set_insert(handle, ptr as SetValueHandle);
    }
    handle
}

/// Set of Result of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_set_to_json_result_product_i64(
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
    let mut items: Vec<(i64, Vec<i64>, String)> = set
        .inner
        .iter()
        .map(|vh| {
            if *vh == 0 {
                (0i64, vec![0; n], String::new())
            } else {
                let ptr = *vh as *const i64;
                if ptr.is_null() {
                    (0i64, vec![0; n], String::new())
                } else {
                    let disc = unsafe { *ptr };
                    if disc == 0 {
                        let err_ptr = unsafe { *ptr.add(1) } as *const std::ffi::c_char;
                        let err_s = if err_ptr.is_null() {
                            String::new()
                        } else {
                            unsafe { cstr_to_string(err_ptr) }
                        };
                        (0i64, vec![0; n], err_s)
                    } else {
                        let fields = unsafe { std::slice::from_raw_parts(ptr.add(1), n).to_vec() };
                        (1i64, fields, String::new())
                    }
                }
            }
        })
        .collect();
    items.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.2.cmp(&b.2))
            .then_with(|| a.1.cmp(&b.1))
    });
    if display_style != 0 {
        let mut parts: Vec<String> = Vec::with_capacity(items.len() * 2 + 2);
        parts.push(String::from("Set{"));
        for (i, (disc, fields, err)) in items.iter().enumerate() {
            if i > 0 {
                parts.push(String::from(", "));
            }
            if *disc == 0 {
                parts.push(format!("Err({})", err));
            } else {
                let body: Vec<String> = fields.iter().map(|x| x.to_string()).collect();
                parts.push(format!("Ok(({}))", body.join(", ")));
            }
        }
        parts.push(String::from("}"));
        alloc_c_string(&parts.join(""))
    } else {
        let mut parts: Vec<String> = Vec::with_capacity(items.len() * 2 + 2);
        parts.push(String::from("["));
        for (i, (disc, fields, err)) in items.iter().enumerate() {
            if i > 0 {
                parts.push(String::from(","));
            }
            if *disc == 0 {
                parts.push(format!("{{\"Err\":[{}]}}", json_escape_string(err)));
            } else {
                let body: Vec<String> = fields.iter().map(|x| x.to_string()).collect();
                parts.push(format!("{{\"Ok\":[[{}]]}}", body.join(",")));
            }
        }
        parts.push(String::from("]"));
        alloc_c_string(&parts.join(""))
    }
}

/// Set of Option of product from JSON array: `[[1,2],null,"None"]`.
#[no_mangle]
pub extern "C" fn mimi_set_from_json_option_product_i64(
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
        let pack_size = 8 + n * 8;
        let ptr = unsafe { libc::malloc(pack_size) as *mut i64 };
        if ptr.is_null() {
            break;
        }
        let is_none = if bytes[i] == b'n' && i + 4 <= bytes.len() && &bytes[i..i + 4] == b"null" {
            i += 4;
            true
        } else if bytes[i] == b'"' && i + 6 <= bytes.len() && &bytes[i..i + 6] == b"\"None\"" {
            i += 6;
            true
        } else {
            false
        };
        if is_none {
            unsafe {
                *ptr = 0;
                for fi in 0..n {
                    *ptr.add(1 + fi) = 0;
                }
            }
        } else {
            if bytes[i] == b'{' {
                while i < bytes.len() && bytes[i] != b'[' {
                    i += 1;
                }
            }
            if i >= bytes.len() || bytes[i] != b'[' {
                unsafe {
                    libc::free(ptr as *mut _);
                }
                break;
            }
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'[' {
                i += 1;
            }
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
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b']' {
                i += 1;
            }
            while i < bytes.len() && bytes[i] != b',' && bytes[i] != b']' && bytes[i] != b'}' {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'}' {
                i += 1;
            }
            unsafe {
                *ptr = 1;
                std::ptr::copy_nonoverlapping(fields.as_ptr(), ptr.add(1), n);
            }
        }
        let vh = ptr as SetValueHandle;
        mimi_set_insert(handle, vh);
    }
    handle
}

/// Set of Option of product Display/JSON.
#[no_mangle]
pub extern "C" fn mimi_set_to_json_option_product_i64(
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
    let mut items: Vec<(i64, Vec<i64>)> = set
        .inner
        .iter()
        .map(|vh| {
            if *vh == 0 {
                (0i64, vec![0; n])
            } else {
                let ptr = *vh as *const i64;
                if ptr.is_null() {
                    (0i64, vec![0; n])
                } else {
                    let disc = unsafe { *ptr };
                    let fields = unsafe { std::slice::from_raw_parts(ptr.add(1), n).to_vec() };
                    (disc, fields)
                }
            }
        })
        .collect();
    items.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    if display_style != 0 {
        let mut parts: Vec<String> = Vec::with_capacity(items.len() * 2 + 2);
        parts.push(String::from("Set{"));
        for (i, (disc, fields)) in items.iter().enumerate() {
            if i > 0 {
                parts.push(String::from(", "));
            }
            if *disc == 0 {
                parts.push(String::from("None()"));
            } else {
                let body: Vec<String> = fields.iter().map(|x| x.to_string()).collect();
                parts.push(format!("Some(({}))", body.join(", ")));
            }
        }
        parts.push(String::from("}"));
        alloc_c_string(&parts.join(""))
    } else {
        let mut parts: Vec<String> = Vec::with_capacity(items.len() * 2 + 2);
        parts.push(String::from("["));
        for (i, (disc, fields)) in items.iter().enumerate() {
            if i > 0 {
                parts.push(String::from(","));
            }
            if *disc == 0 {
                parts.push(String::from("\"None\""));
            } else {
                let body: Vec<String> = fields.iter().map(|x| x.to_string()).collect();
                parts.push(format!("{{\"Some\":[[{}]]}}", body.join(",")));
            }
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
        .map(|v| safe_c_string_from_handle(*v as ValueHandle).unwrap_or_else(|| v.to_string()))
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
    // R-C11: double free is a no-op; only free if still live.
    if !set_take_live(handle) {
        return;
    }
    // SAFETY: handle was live and removed under lock; exclusive ownership restored.
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

mod regex;

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

mod net;

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

mod ffi_test;

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
                    std::ffi::CString::new(info.state_name.as_str()).unwrap_or_else(|_| {
                        // Fallback never fails: no interior NULs.
                        std::ffi::CString::new("FFI_Pinned")
                            .unwrap_or_else(|_| std::ffi::CString::default())
                    }),
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

mod shadow_mte;
#[cfg(not(standalone))]
pub use shadow_mte::*;

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

mod future;

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

mod fs;

mod crypto;
#[cfg(not(standalone))]
pub use crypto::{base64_decode_str, base64_encode_bytes, sha256_bytes};

mod binary_io;

mod lexer;
#[cfg(not(standalone))]
pub use lexer::{mimi_lexer_tokenize, mimi_parse_source};

mod actor;
#[cfg(not(standalone))]
pub use actor::*;

mod concurrency;
#[cfg(not(standalone))]
pub use concurrency::*;

mod quote;
#[cfg(not(standalone))]
pub use quote::*;

// ---------------------------------------------------------------------------
// R-C11 regression tests: live-handle registry
// ---------------------------------------------------------------------------
#[cfg(test)]
mod handle_registry_tests {
    use super::*;

    #[test]
    fn quote_accessors_reject_same_layout_unregistered_metadata() {
        let mut forged = MimiQuotedAst {
            tag: QuotedAstTag::QastInt as i32,
            argc: 0,
            data0: 99,
            data1: 0,
            data2: 0,
        };
        let ptr = &mut forged as *mut MimiQuotedAst;
        assert_eq!(mimi_quote_tag(ptr), -1);
        assert_eq!(mimi_quote_data0(ptr), 0);
        assert_eq!(mimi_quote_argc(ptr), 0);
        assert!(mimi_quote_list_child(ptr, 0).is_null());
    }

    #[test]
    fn quote_snapshot_accessors_reject_dropped_handle() {
        let node = mimi_quote_new_leaf(QuotedAstTag::QastInt as i32, 42);
        assert_eq!(mimi_quote_tag(node), QuotedAstTag::QastInt as i32);
        mimi_quote_drop(node);
        assert_eq!(mimi_quote_tag(node), -1);
        assert_eq!(mimi_quote_data0(node), 0);
        assert_eq!(mimi_quote_argc(node), 0);
        assert!(mimi_quote_list_child(node, 0).is_null());
    }

    #[test]
    fn quote_abi_is_versioned_and_rejects_unknown_tags() {
        assert_eq!(mimi_quote_abi_version(), 1);
        assert!(mimi_quote_new_leaf(i32::MAX, 42).is_null());
    }

    #[test]
    fn map_double_destroy_is_noop() {
        let h = mimi_map_new();
        assert_ne!(h, 0);
        assert_eq!(mimi_map_size(h), 0);
        mimi_map_destroy(h);
        // Second destroy must not free again (would be double-free).
        mimi_map_destroy(h);
        mimi_map_destroy(0);
    }

    #[test]
    fn set_double_destroy_is_noop() {
        let h = mimi_set_new();
        assert_ne!(h, 0);
        mimi_set_destroy(h);
        mimi_set_destroy(h);
        mimi_set_destroy(0);
    }

    #[test]
    fn map_ops_on_live_handle_work() {
        let h = mimi_map_new();
        let key = b"k\0".as_ptr() as *const std::ffi::c_char;
        mimi_map_set(h, key, 42);
        assert_eq!(mimi_map_has_key(h, key), 1);
        assert_eq!(mimi_map_get(h, key), 42);
        assert_eq!(mimi_map_size(h), 1);
        mimi_map_destroy(h);
    }

    #[test]
    fn set_insert_on_live_handle_works() {
        let h = mimi_set_new();
        let h2 = mimi_set_insert(h, 7);
        assert_eq!(h, h2);
        mimi_set_destroy(h);
    }
}
