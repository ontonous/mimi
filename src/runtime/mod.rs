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

/// v0.28.13: Allocate a MimiList data array with hidden capacity header at data[-8].
/// The header uses bit 63 as a magic marker: `(i64::MIN | cap)`.
/// Returns the data pointer (header is at data[-8]). Null on failure.
fn alloc_list_data(cap: i64) -> *mut *mut std::ffi::c_char {
    if cap <= 0 {
        return std::ptr::null_mut();
    }
    let sz = 8 + (cap as usize) * std::mem::size_of::<*mut std::ffi::c_char>();
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

/// v0.28.13: Reallocate a MimiList data array, preserving the hidden capacity header.
fn realloc_list_data(old: *mut *mut std::ffi::c_char, new_cap: i64) -> *mut *mut std::ffi::c_char {
    if new_cap <= 0 {
        return std::ptr::null_mut();
    }
    let sz = 8 + (new_cap as usize) * std::mem::size_of::<*mut std::ffi::c_char>();
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

/// v0.28.13: Read the hidden capacity from data[-8]. Returns 0 if no header.
fn list_cap(data: *mut *mut std::ffi::c_char) -> i64 {
    if data.is_null() {
        return 0;
    }
    // SAFETY: reading data[-1] is valid only for Rust-allocated lists with the hidden header; otherwise returns 0.
    let hdr = unsafe { *(data as *mut i64).offset(-1) };
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
    // SAFETY: `list` was checked non-null; mutable reference is held only within this function.
    let lst = unsafe { &mut *list };
    let len = lst.len;
    let cap = list_cap(lst.data);
    if len + 1 > cap {
        let nc = if cap <= 0 { 4 } else { cap * 2 };
        let nd = realloc_list_data(lst.data, nc);
        if nd.is_null() {
            return;
        }
        lst.data = nd;
        // SAFETY: after growth `nd` has capacity >= `len + 1`; writing at index `len` is in bounds.
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
    let cap = list_cap(old_data);
    let needed = len + additional;
    if needed > cap {
        let new_cap = if cap <= 0 {
            if needed < 4 {
                4
            } else {
                needed
            }
        } else {
            let mut nc = cap;
            while nc < needed {
                nc *= 2;
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
            let copy_size = (len as usize) * std::mem::size_of::<*mut std::ffi::c_char>();
            // SAFETY: existing elements are copied byte-for-byte from the old buffer to the new buffer.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    old_data as *const u8,
                    new_data as *mut u8,
                    copy_size,
                );
            }
        }
        // Free old data if it has a header; otherwise free directly
        if cap > 0 {
            // Has header: free allocation base (data - 8)
            // SAFETY: `old_data` has a hidden header, so `old_data - 1` is the valid allocation base.
            let base = unsafe { (old_data as *mut i64).offset(-1) as *mut std::ffi::c_void };
            unsafe {
                // SAFETY: `base` is the valid allocation base returned by `alloc_list_data`.
                libc::free(base);
            }
        } else if !old_data.is_null() {
            // No header (literal): free data directly
            unsafe {
                // SAFETY: `old_data` is a non-null allocation without a header, freed directly.
                libc::free(old_data as *mut std::ffi::c_void);
            }
        }
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

/// S22: Free a MimiList and optionally its C string elements.
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
    unsafe {
        // SAFETY: `list` was checked non-null; data is freed using the correct base depending on header presence.
        let lst = &*list;
        if lst.owns_data && !lst.data.is_null() {
            let cap = list_cap(lst.data);
            if cap > 0 {
                if free_elements {
                    for i in 0..lst.len as usize {
                        // SAFETY: element pointer is non-null and was allocated by `libc::malloc`.
                        let e = *lst.data.add(i);
                        if !e.is_null() {
                            libc::free(e as *mut std::ffi::c_void);
                        }
                    }
                }
                // SAFETY: `cap > 0` guarantees a hidden header at `data - 1`.
                let base = (lst.data as *mut i64).offset(-1) as *mut std::ffi::c_void;
                libc::free(base);
            } else {
                if free_elements {
                    for i in 0..lst.len as usize {
                        // SAFETY: element pointer is non-null and was allocated by `libc::malloc`.
                        let e = *lst.data.add(i);
                        if !e.is_null() {
                            libc::free(e as *mut std::ffi::c_void);
                        }
                    }
                }
                // SAFETY: `data` is non-null and has no hidden header; free the pointer directly.
                libc::free(lst.data as *mut std::ffi::c_void);
            }
        }
        // SAFETY: `list` was checked non-null and is freed after its data/elements.
        libc::free(list as *mut std::ffi::c_void);
    }
}

/// Allocate a C string from bytes that already include the null terminator.
fn alloc_c_string_from_bytes(bytes: &[u8]) -> *mut std::ffi::c_char {
    if bytes.is_empty() {
        // SAFETY: allocating one byte and writing the null terminator.
        let ptr = unsafe { libc::malloc(1) as *mut u8 };
        if !ptr.is_null() {
            unsafe {
                // SAFETY: writing the null terminator within the single-byte allocation.
                *ptr = 0;
            }
        }
        return ptr as *mut std::ffi::c_char;
    }
    // SAFETY: `bytes.len()` is non-zero here; allocation size matches copy length.
    let ptr = unsafe { libc::malloc(bytes.len()) as *mut u8 };
    if ptr.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: source and destination are non-overlapping and `bytes.len()` fits in the allocation.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
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
    if unsafe { (*hdr).strong.fetch_sub(1, Ordering::Release) } == 1 {
        std::sync::atomic::fence(Ordering::Acquire);
        // SAFETY: after the Acquire fence, reading weak count is synchronized with the releasing thread.
        if unsafe { (*hdr).weak.load(Ordering::Relaxed) } == 0 {
            let layout = unsafe { rc_dealloc_layout(hdr) };
            unsafe {
                // SAFETY: `layout` is reconstructed from the valid header; dealloc matches the original alloc.
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
        // SAFETY: after the Acquire fence, reading strong count is synchronized with releasing threads.
        if unsafe { (*hdr).strong.load(Ordering::Relaxed) } <= 0 {
            let layout = unsafe { rc_dealloc_layout(hdr) };
            unsafe {
                // SAFETY: `layout` is reconstructed from the valid header; dealloc matches the original alloc.
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
    let mut s = hdr.strong.load(Ordering::Relaxed);
    loop {
        if s == 0 {
            return std::ptr::null_mut();
        }
        match hdr
            .strong
            .compare_exchange_weak(s, s + 1, Ordering::Acquire, Ordering::Relaxed)
        {
            Ok(_) => return ptr,
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
pub extern "C" fn mimi_map_from_list(
    keys: *mut ValueHandle,
    values: *mut ValueHandle,
    n: i64,
) -> MapHandle {
    let handle = mimi_map_new();
    if handle == 0 || keys.is_null() || values.is_null() || n <= 0 {
        return handle;
    }
    // S6: Validate n doesn't exceed reasonable bounds (max 1M entries).
    let n = n.min(1_000_000);
    for i in 0..n {
        // SAFETY: caller must ensure `keys`/`values` arrays have at least `n` elements.
        unsafe {
            let key_handle = *keys.add(i as usize);
            let val_handle = *values.add(i as usize);
            let key_str = key_handle as *const std::ffi::c_char;
            if !key_str.is_null() {
                let s = CStr::from_ptr(key_str).to_str().unwrap_or("");
                (*map_from_handle(handle))
                    .inner
                    .insert(s.to_string(), val_handle);
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

    let mut items: Vec<*mut std::ffi::c_char> = Vec::with_capacity(len as usize);
    for (k, v) in &map.inner {
        if collect_values {
            // S10: ValueHandle is an opaque integer; cast to pointer for FFI transport.
            // Caller must NOT free these pointers — they are not heap-allocated strings.
            let val_ptr = *v as *mut std::ffi::c_char;
            items.push(val_ptr);
        } else {
            items.push(alloc_c_string(k.as_str()));
        }
    }

    let data_ptr = items.as_mut_ptr();
    // FFI-2: If collect_values=true, data contains opaque handles (not owned by Rust).
    // If collect_values=false, data contains alloc_c_string results (owned by Rust).
    std::mem::forget(items);
    let list = Box::new(MimiList {
        len,
        data: data_ptr,
        owns_data: !collect_values, // only own data when strings are allocated
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

    let parts: Vec<String> = if d.is_empty() {
        // Empty delimiter: split into individual characters
        if ss.is_empty() {
            vec!["".to_string()]
        } else {
            ss.chars().map(|c| c.to_string()).collect()
        }
    } else {
        ss.split(&d).map(|p| p.to_string()).collect()
    };

    let len = parts.len() as i64;
    let mut c_strings: Vec<*mut std::ffi::c_char> =
        parts.into_iter().map(|p| alloc_c_string(&p)).collect();
    let data_ptr = c_strings.as_mut_ptr();
    // FFI-11: Use ManuallyDrop instead of mem::forget to avoid leaking Vec metadata.
    let _ = std::mem::ManuallyDrop::new(c_strings);

    // FFI-2: Strings are allocated via alloc_c_string (libc::malloc) — owns_data: true.
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

/// Render a codegen `List<i32>` (layout `{i64 len, i8* data}` where data points
/// to i64 slots) to a printable heap-allocated C string.
#[no_mangle]
pub extern "C" fn mimi_list_i32_to_string(list: *const MimiList) -> *mut std::ffi::c_char {
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
        // `lst.data` was bitcast from `*mut i64`; cast back and read element.
        let item = unsafe { *(lst.data as *const i64).offset(i) };
        parts.push(item.to_string());
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
        // `lst.data` points to inner list pointers (`*const MimiList`).
        let inner = unsafe { *lst.data.offset(i) as *const MimiList };
        if inner.is_null() {
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
        std::thread::sleep(std::time::Duration::from_millis(ms as u64));
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
    let args_mutex = CLI_ARGS.get().expect("CLI_ARGS not initialized");
    let mut args = args_mutex.lock().unwrap_or_else(|e| e.into_inner());
    args.argc = argc;
    args.argv.clear();
    // S9: Copy strings to owned memory instead of storing raw pointers.
    // Original argv may be freed after init returns.
    if !argv.is_null() && argc > 0 {
        for i in 0..argc as isize {
            // SAFETY: pointer is non-null or the helper handles null safely.
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
    let args_mutex = CLI_ARGS.get().expect("CLI_ARGS not initialized");
    let args = args_mutex.lock().unwrap_or_else(|e| e.into_inner());
    if args.argc <= 1 {
        return 0;
    }
    (args.argc - 1) as i64
}

#[no_mangle]
pub extern "C" fn mimi_args_list() -> *mut MimiList {
    init_cli_args();
    let args_mutex = CLI_ARGS.get().expect("CLI_ARGS not initialized");
    let args = args_mutex.lock().unwrap_or_else(|e| e.into_inner());
    let count = if args.argc <= 1 {
        0
    } else {
        (args.argc - 1) as usize
    };
    let mut items: Vec<*mut std::ffi::c_char> = Vec::with_capacity(count);
    for i in 1..args.argc as usize {
        items.push(args.argv[i] as *mut std::ffi::c_char);
    }
    let data_ptr = items.as_mut_ptr();
    let len = items.len() as i64;
    std::mem::forget(items);
    Box::into_raw(Box::new(MimiList {
        len,
        data: data_ptr,
        owns_data: false,
    }))
}

#[no_mangle]
pub extern "C" fn mimi_args_get(i: i64) -> *mut std::ffi::c_char {
    init_cli_args();
    let args_mutex = CLI_ARGS.get().expect("CLI_ARGS not initialized");
    let args = args_mutex.lock().unwrap_or_else(|e| e.into_inner());
    if i < 0 || i >= (args.argc - 1) as i64 {
        return std::ptr::null_mut();
    }
    let idx = (i + 1) as usize; // +1 to skip program name
    args.argv
        .get(idx)
        .copied()
        .map(|p| p as *mut std::ffi::c_char)
        .unwrap_or(std::ptr::null_mut())
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
                        if self.p[self.pos] == b'\\' {
                            self.pos += 2;
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
                        if self.p[self.pos] == b'\\' {
                            self.pos += 2;
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

#[no_mangle]
pub extern "C" fn json_get_int(
    json_str: *const std::ffi::c_char,
    key: *const std::ffi::c_char,
) -> i64 {
    match json_get_inner(json_str, key) {
        Some(val) => val.parse::<i64>().unwrap_or(0),
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

#[no_mangle]
pub extern "C" fn mimi_json_as_i64(json: *const std::ffi::c_char) -> i64 {
    if json.is_null() {
        return 0;
    }
    // SAFETY: `json` was checked non-null above.
    let s = unsafe { cstr_to_string(json) };
    let mut parser = JsonParser::new(&s);
    match parser.parse_value() {
        Some(val) => val.parse::<i64>().unwrap_or(0),
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

type SetHandle = i64;
type SetValueHandle = i64;

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
    // Invalid handle: returns -1 cast to pointer, *out_len = -1.
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
        return -1isize as *mut SetValueHandle;
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
    vec.shrink_to_fit();
    let ptr = vec.as_mut_ptr();
    std::mem::forget(vec); // ownership transferred to caller
    ptr
}

// ─── Regex (simple recursive backtracking engine, self-contained) ───

struct RegexEngine;

/// S17: Maximum recursion depth for regex backtracking to prevent ReDoS.
/// Patterns like `(a+)+b` on `aaaaaaaaaaaaaaaac` cause exponential recursion.
const REGEX_MAX_DEPTH: usize = 100;

impl RegexEngine {
    fn match_pattern(text: &str, pattern: &str) -> bool {
        let text_bytes = text.as_bytes();
        let pat_bytes = pattern.as_bytes();
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

    fn find_match(text: &str, pattern: &str) -> Option<(usize, usize)> {
        let text_bytes = text.as_bytes();
        let pat_bytes = pattern.as_bytes();
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
        let text_bytes = text.as_bytes();
        let pat_bytes = pattern.as_bytes();
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
/// Returns a JSON array of capture group values: ["group1","group2",...]
/// NOTE: The custom RegexEngine does not support capture groups.
/// This returns "[]" for all inputs. Use from interpreter with regex crate for full support.
#[no_mangle]
pub extern "C" fn mimi_regex_capture_groups(
    #[allow(unused_variables)] text: *const std::ffi::c_char,
    #[allow(unused_variables)] pattern: *const std::ffi::c_char,
) -> *mut std::ffi::c_char {
    // The custom RegexEngine doesn't support capture groups.
    // Return empty array. Interpreter path uses regex crate for full support.
    alloc_c_string("[]")
}

// ─── Sort helpers ────────────────────────────────────────────────

/// Sorts an f64 list in place (ascending). data points to the raw element buffer.
/// count is the number of elements. Each f64 is 8 bytes (stored as i64 bits).
#[no_mangle]
pub extern "C" fn mimi_sort_f64_inplace(data: *mut u8, count: i64) {
    if data.is_null() || count <= 1 {
        return;
    }
    let elem_size: usize = 8;
    let total_bytes = (count as usize) * elem_size;
    // SAFETY: `data` is non-null and caller must ensure it points to `count * 8` writable bytes.
    let slice = unsafe { std::slice::from_raw_parts_mut(data, total_bytes) };
    for i in 0..(count as usize) {
        for j in 0..(count as usize) - 1 - i {
            let a_off = j * elem_size;
            let b_off = (j + 1) * elem_size;
            let a_bits = u64::from_ne_bytes([
                slice[a_off],
                slice[a_off + 1],
                slice[a_off + 2],
                slice[a_off + 3],
                slice[a_off + 4],
                slice[a_off + 5],
                slice[a_off + 6],
                slice[a_off + 7],
            ]);
            let b_bits = u64::from_ne_bytes([
                slice[b_off],
                slice[b_off + 1],
                slice[b_off + 2],
                slice[b_off + 3],
                slice[b_off + 4],
                slice[b_off + 5],
                slice[b_off + 6],
                slice[b_off + 7],
            ]);
            if f64::from_bits(a_bits) > f64::from_bits(b_bits) {
                for k in 0..elem_size {
                    slice.swap(a_off + k, b_off + k);
                }
            }
        }
    }
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
    // Bubble sort: stable, O(n^2) but fine for typical small lists.
    for i in 0..n {
        for j in 0..(n - 1 - i) {
            let a = slice[j];
            let b = slice[j + 1];
            if a.is_null() || b.is_null() {
                // Treat null as greater than any real string to keep them at the tail.
                if a.is_null() && !b.is_null() {
                    slice.swap(j, j + 1);
                }
                continue;
            }
            // SAFETY: `a` is non-null (checked above).
            let a_str = unsafe { CStr::from_ptr(a) };
            // SAFETY: `b` is non-null (checked above).
            let b_str = unsafe { CStr::from_ptr(b) };
            if a_str > b_str {
                slice.swap(j, j + 1);
            }
        }
    }
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
    // We'll use libc calls directly.
    // SAFETY: direct POSIX socket calls; integer arguments are cast from validated i64 values.
    unsafe {
        let fd = libc::socket(domain as i32, type_ as i32, protocol as i32);
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
    // SAFETY: direct POSIX calls with a validated file descriptor.
    unsafe {
        let fd_i32 = match fd_to_i32(fd) {
            Some(v) => v,
            None => return -1,
        };
        let mut addr: libc::sockaddr_in = std::mem::zeroed();
        addr.sin_family = libc::AF_INET as libc::sa_family_t;
        addr.sin_port = (port as u16).to_be();
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
    let rest = url.strip_prefix("http://")?;
    if url.starts_with("https://") {
        return None;
    }

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
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));

    // Send request
    use std::io::Write;
    if let Err(e) = stream.write_all(request.as_bytes()) {
        eprintln!("[mimi runtime] HTTP write error: {}", e);
        return None;
    }

    // Read response
    let mut response = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => response.extend_from_slice(&buf[..n]),
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
        None => return std::ptr::null_mut(),
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
        None => alloc_c_string(""),
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
        None => return std::ptr::null_mut(),
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
        None => alloc_c_string(""),
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
                // String: raw is a C string pointer
                result.push('"');
                if raw != 0 {
                    // SAFETY: `raw` is non-zero and the caller must ensure it is a valid C string pointer.
                    let s = unsafe { std::ffi::CStr::from_ptr(raw as *const std::ffi::c_char) };
                    let s_str = s.to_string_lossy();
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
                    if bytes[p] == b'\\' {
                        p += 2;
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
                while pos < bytes.len() && bytes[pos] != b'"' {
                    if bytes[pos] == b'\\' {
                        pos += 2;
                    } else {
                        pos += 1;
                    }
                }
                let slen = pos - start;
                let s_bytes = bytes[start..start + slen].to_vec();
                data[idx as usize] = alloc_c_string_from_bytes(&s_bytes) as i64;
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
                    // Free any strings allocated so far and return null
                    if elem_type == 2 {
                        for i in 0..idx {
                            let ptr = data[i as usize] as *mut std::ffi::c_char;
                            if !ptr.is_null() {
                                // SAFETY: freeing a non-null string allocated during deserialization.
                                unsafe { libc::free(ptr as *mut std::ffi::c_void) };
                            }
                        }
                    }
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

    let result = data.as_mut_ptr();
    std::mem::forget(data);
    if !out_len.is_null() {
        // SAFETY: `out_len` was checked non-null above.
        unsafe {
            *out_len = idx;
        }
    }
    result as *mut std::ffi::c_void
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
                result.push('"');
                if raw != 0 {
                    // SAFETY: `raw` is non-zero and caller must ensure it is a valid C string pointer.
                    let s = unsafe { std::ffi::CStr::from_ptr(raw as *const std::ffi::c_char) };
                    let s_str = s.to_string_lossy();
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
                while pos < bytes.len() && bytes[pos] != b'"' {
                    if bytes[pos] == b'\\' {
                        pos += 2;
                    } else {
                        pos += 1;
                    }
                }
                let slen = pos - start;
                if slen > 0 {
                    let s_bytes = bytes[start..start + slen].to_vec();
                    unsafe {
                        // SAFETY: `out_values` was checked non-null above; `idx < count`.
                        *out_values.offset(idx as isize) =
                            alloc_c_string_from_bytes(&s_bytes) as i64;
                    }
                } else {
                    unsafe {
                        // SAFETY: `out_values` was checked non-null above; `idx < count`.
                        *out_values.offset(idx as isize) = 0;
                    }
                }
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
                while pos < bytes.len() && bytes[pos].is_ascii_digit() {
                    val = val
                        .wrapping_mul(10)
                        .wrapping_add((bytes[pos] - b'0') as i64);
                    pos += 1;
                }
                if neg {
                    val = val.wrapping_neg();
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
        .unwrap_or(0);
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

// FFI-4: The UB trigger __mimi_extern_test_segfault is always compiled into the
// staticlib. It ALWAYS performs the UB (no cfg gate). The test wrapper
// test_segfault is gated #[cfg(test)] so only Mimi test code can trigger it.
#[no_mangle]
pub extern "C" fn __mimi_extern_test_segfault() {
    // Deliberate null pointer dereference — used by FFI safety tests to verify
    // crash handling. In non-test builds this is never called (test_segfault
    // wrapper is gated #[cfg(test)]).
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
        None => i64::MIN as i32,
    });
    handle.join().unwrap_or(i64::MIN as i32)
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
    // SAFETY: writing static byte buffers to stderr (fd 2) is async-signal-safe.
    unsafe {
        let _ = write(2, PREFIX.as_ptr() as *const std::ffi::c_void, PREFIX.len());
        if !msg.is_null() {
            // SAFETY: `msg` was checked non-null above.
            let s = CStr::from_ptr(msg);
            let loss = s.to_string_lossy();
            let bytes = loss.as_bytes();
            let _ = write(2, bytes.as_ptr() as *const std::ffi::c_void, bytes.len());
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
        let mut state = table.lock().expect("cap table lock poisoned");
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
    // SAFETY: `fut` was checked non-null; `MimiFutureRepr` is valid.
    unsafe {
        let rep = &*(fut as *const MimiFutureRepr);
        rep.completed.store(1, Ordering::Release);
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
/// Joining detached threads avoids Valgrind "possibly lost" reports for the
/// pthread stack allocation. The mutex protects the vector across threads.
static SPAWN_HANDLES: std::sync::Mutex<Vec<std::thread::JoinHandle<()>>> =
    std::sync::Mutex::new(Vec::new());
static SPAWN_ATEXIT_REGISTERED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

extern "C" fn mimi_join_spawned_threads_atexit() {
    // SAFETY: called once from `atexit` after main returns; no other thread is
    // accessing `SPAWN_HANDLES` at this point (all futures have completed).
    if let Ok(mut handles) = SPAWN_HANDLES.lock() {
        for handle in handles.drain(..) {
            let _ = handle.join();
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
    if let Ok(mut handles) = SPAWN_HANDLES.lock() {
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
    let mut queue = EXECUTOR_QUEUE.lock().expect("executor queue lock poisoned");
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
            let mut queue = EXECUTOR_QUEUE.lock().expect("executor queue lock poisoned");
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
        let state = table.lock().expect("cap table lock poisoned");
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
        let mut state = table.lock().expect("cap table lock poisoned");
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
/// Caller must free with `mimi_exec_free`.
#[no_mangle]
pub extern "C" fn mimi_exec(cmd: *const std::ffi::c_char) -> *mut MimiExecResult {
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
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd_str)
        .output();
    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
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
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd_str)
        .output();
    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            alloc_c_string(&stdout)
        }
        Err(_) => alloc_c_string(""),
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

/// Sets an environment variable. Returns 1 on success, 0 on failure.
#[no_mangle]
pub extern "C" fn mimi_set_env(
    key: *const std::ffi::c_char,
    value: *const std::ffi::c_char,
) -> i64 {
    if key.is_null() || value.is_null() {
        return 0;
    }
    // SAFETY: `key` was checked non-null above.
    let key_str = match unsafe { CStr::from_ptr(key) }.to_str() {
        Ok(s) => s,
        Err(_) => return 0,
    };
    // SAFETY: `value` was checked non-null above.
    let value_str = match unsafe { CStr::from_ptr(value) }.to_str() {
        Ok(s) => s,
        Err(_) => return 0,
    };
    // SAFETY: set_var is unsafe in newer Rust editions; safe for single-threaded Mimi programs
    unsafe { std::env::set_var(key_str, value_str) };
    1
}

// ─── Crypto operations ─────────────────────────────────────────

/// SHA-256 hash — returns hex string (64 chars).
/// Pure Rust implementation, no external dependencies.
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

/// Reads an entire file as raw bytes, returned as a C string (may contain null bytes).
/// Caller must free with mimi_string_free.
#[no_mangle]
pub extern "C" fn mimi_read_file_bytes(path: *const std::ffi::c_char) -> *mut std::ffi::c_char {
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
            let s = String::from_utf8_lossy(&bytes);
            alloc_c_string(&s)
        }
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
/// Returns the number of lines processed, or -1 on error.
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

    let dispatch = dispatch_fn.expect("dispatch_fn checked non-null at entry");
    let worker_id = id;

    let handle = std::thread::Builder::new()
        .name(format!("mimi-actor-{}", id))
        .spawn(move || {
            // Set the thread-local so self-call detection works.
            CURRENT_ACTOR_ID.with(|c| c.set(worker_id));

            // The worker owns the mutable field storage.
            let mut fields = worker_fields;

            // Result blob is allocated once and reused.
            let mut result_blob = vec![0u8; MIMI_ACTOR_BLOB_SIZE];

            while let Ok(msg) = rx.recv() {
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
        })
        .expect("failed to spawn actor worker thread");

    let repr = Box::new(MimiActorRepr {
        id,
        fields: Box::new([]), // handle doesn't own live fields; worker does
        mailbox_tx: tx,
        worker: Some(handle),
    });

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

    // Send to mailbox. If the channel is closed (actor dropped), return 0.
    if repr.mailbox_tx.send(msg).is_err() {
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

/// A held mutex guard returned to codegen/interpreter as an opaque guard
/// handle. The `_arc` clone keeps the `Mutex` alive for the guard's lifetime;
/// the `guard` lifetime is extended to `'static` because the mutex will not be
/// deallocated while this struct exists.
struct HeldMutexGuard {
    _arc: Arc<std::sync::Mutex<i64>>,
    guard: std::sync::MutexGuard<'static, i64>,
}

thread_local! {
    /// Guards are stored per-thread because `std::sync::MutexGuard` is not
    /// `Send`. A mutex guard should only be accessed from the thread that
    /// locked it, so thread-local storage is both safe and semantically correct.
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
        Some(ConcurrencyAtomic::Bool(a)) => {
            if a.load(std::sync::atomic::Ordering::SeqCst) {
                1
            } else {
                0
            }
        }
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
    // SAFETY: the `Arc` clone in `HeldMutexGuard` keeps the mutex alive for as
    // long as the guard exists, so extending the guard lifetime to `'static`
    // is sound. The guard is stored in thread-local storage until
    // `mimi_mutex_unlock` removes it.
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
            let result = rx.recv().unwrap_or_default();
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
    *rx_arc.lock().unwrap_or_else(|e| e.into_inner()) = None;
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
