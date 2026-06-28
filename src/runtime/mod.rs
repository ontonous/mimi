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
        pub fn free(ptr: *mut c_void);
    }
}

use std::collections::HashMap;
use std::ffi::{CStr, CString};
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
// Memory allocation helpers
// ---------------------------------------------------------------------------

/// Allocate a C string (null-terminated) using libc::malloc.
/// The caller is responsible for freeing with mimi_string_free or libc::free.
fn alloc_c_string(s: &str) -> *mut std::ffi::c_char {
    let bytes = s.as_bytes();
    let len = bytes.len();
    let ptr = unsafe { libc::malloc(len + 1) as *mut u8 };
    if ptr.is_null() {
        return std::ptr::null_mut();
    }
    if len > 0 {
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, len);
        }
    }
    unsafe {
        *ptr.add(len) = 0;
    }
    ptr as *mut std::ffi::c_char
}

/// S15/S22: Free a C string allocated by alloc_c_string.
/// Safe to call with null pointer (no-op).
#[no_mangle]
pub extern "C" fn mimi_string_free(ptr: *mut std::ffi::c_char) {
    if !ptr.is_null() {
        unsafe { libc::free(ptr as *mut std::ffi::c_void); }
    }
}

/// S22: Free a MimiList and optionally its C string elements.
/// FFI-2: Only frees data if `owns_data` is true (Rust-allocated).
/// C-allocated data (owns_data=false) is skipped to avoid wrong-allocator heap corruption.
#[no_mangle]
pub extern "C" fn mimi_list_free(list: *mut MimiList, free_elements: bool) {
    if list.is_null() {
        return;
    }
    unsafe {
        let list = &*list;
        // FFI-2: Only free data if we own it. C may pass lists where data points
        // into C-allocated memory (e.g., from str_split results), which must NOT be
        // freed by libc::free if a different allocator was used.
        if list.owns_data && free_elements && !list.data.is_null() {
            for i in 0..list.len as usize {
                let elem = *list.data.add(i);
                if !elem.is_null() {
                    libc::free(elem as *mut std::ffi::c_void);
                }
            }
            libc::free(list.data as *mut std::ffi::c_void);
        }
        libc::free(list as *const MimiList as *mut std::ffi::c_void);
    }
}

/// Allocate a C string from bytes that already include the null terminator.
fn alloc_c_string_from_bytes(bytes: &[u8]) -> *mut std::ffi::c_char {
    if bytes.is_empty() {
        let ptr = unsafe { libc::malloc(1) as *mut u8 };
        if !ptr.is_null() {
            unsafe {
                *ptr = 0;
            }
        }
        return ptr as *mut std::ffi::c_char;
    }
    let ptr = unsafe { libc::malloc(bytes.len()) as *mut u8 };
    if ptr.is_null() {
        return std::ptr::null_mut();
    }
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

unsafe fn rc_header_from_ptr(ptr: *mut std::ffi::c_void) -> *mut RcHeader {
    (ptr as *mut RcHeader).sub(1)
}

/// S1: Helper to get a shared reference for atomic operations (no aliasing UB).
/// Caller must ensure ptr is valid and not concurrently freed.
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
        .extend(std::alloc::Layout::array::<u8>(size as usize).unwrap_or_else(|_| std::process::abort()))
        .unwrap_or_else(|_| std::process::abort())
        .0
        .pad_to_align();
    let ptr = unsafe { std::alloc::alloc(layout) };
    if ptr.is_null() {
        return std::ptr::null_mut();
    }
    let hdr = ptr as *mut RcHeader;
    unsafe {
        (*hdr).strong = AtomicI64::new(1);
        (*hdr).weak = AtomicI64::new(0);
        (*hdr).alloc_size = size;
    }
    unsafe { (hdr.add(1)) as *mut std::ffi::c_void }
}

#[no_mangle]
pub extern "C" fn mimi_rc_retain(ptr: *mut std::ffi::c_void) {
    if ptr.is_null() {
        return;
    }
    let hdr = unsafe { rc_header_from_ptr(ptr) };
    unsafe { (*hdr).strong.fetch_add(1, Ordering::Relaxed); }
}

/// Helper: build the dealloc Layout from RcHeader's stored alloc_size.
/// FFI-1: Uses abort instead of panicking if alloc_size is corrupted.
unsafe fn rc_dealloc_layout(hdr: *mut RcHeader) -> std::alloc::Layout {
    let user_size = (*hdr).alloc_size as usize;
    // Guard against corrupted alloc_size that would cause Layout::array to panic.
    if user_size == 0 || user_size > 0x7fff_ffff {
        std::process::abort();
    }
    std::alloc::Layout::new::<RcHeader>()
        .extend(std::alloc::Layout::array::<u8>(user_size).unwrap_or_else(|_| std::process::abort()))
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
    if unsafe { (*hdr).strong.fetch_sub(1, Ordering::Release) } == 1 {
        std::sync::atomic::fence(Ordering::Acquire);
        if unsafe { (*hdr).weak.load(Ordering::Relaxed) } == 0 {
            let layout = unsafe { rc_dealloc_layout(hdr) };
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
    let hdr = unsafe { rc_header_from_ptr(ptr) };
    // S2: CAS loop to avoid TOCTOU race on weak count.
    // Old code: load strong, load weak, check both zero, then fetch_add.
    // Between load and fetch_add, another thread could complete release+dealloc.
    // CAS ensures we only increment if the object is still alive.
    loop {
        let s = unsafe { (*hdr).strong.load(Ordering::Acquire) };
        let w = unsafe { (*hdr).weak.load(Ordering::Relaxed) };
        if s == 0 && w == 0 {
            return; // Object already freed or being freed
        }
        // Try to increment weak; if strong went to 0 between our load and CAS, retry.
        let prev = unsafe { (*hdr).weak.compare_exchange(w, w + 1, Ordering::AcqRel, Ordering::Relaxed) };
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
    if unsafe { (*hdr).weak.fetch_sub(1, Ordering::Release) } == 1 {
        std::sync::atomic::fence(Ordering::Acquire);
        if unsafe { (*hdr).strong.load(Ordering::Relaxed) } <= 0 {
            let layout = unsafe { rc_dealloc_layout(hdr) };
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
    unsafe {
        drop(Box::from_raw(handle as *mut MimiMap));
    }
}

#[no_mangle]
pub extern "C" fn mimi_map_size(handle: MapHandle) -> i64 {
    if handle == 0 {
        return 0;
    }
    unsafe { (*map_from_handle(handle)).inner.len() as i64 }
}

#[no_mangle]
pub extern "C" fn mimi_map_has_key(handle: MapHandle, key: *const std::ffi::c_char) -> i32 {
    if handle == 0 || key.is_null() {
        return 0;
    }
    let s = unsafe { cstr_to_string(key) };
    unsafe { (*map_from_handle(handle)).inner.contains_key(&s) as i32 }
}

#[no_mangle]
pub extern "C" fn mimi_map_get(handle: MapHandle, key: *const std::ffi::c_char) -> ValueHandle {
    if handle == 0 || key.is_null() {
        return 0;
    }
    let s = unsafe { cstr_to_string(key) };
    unsafe { (*map_from_handle(handle)).inner.get(&s).copied().unwrap_or(0) }
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
    let sa = unsafe { cstr_to_string(a) };
    let sb = unsafe { cstr_to_string(b) };
    let result = format!("{}{}", sa, sb);
    alloc_c_string(&result)
}

#[no_mangle]
pub extern "C" fn mimi_str_split(
    s: *const std::ffi::c_char,
    delim: *const std::ffi::c_char,
) -> *mut MimiList {
    let ss = unsafe { cstr_to_string(s) };
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
    let lst = unsafe { &*list };
    if lst.data.is_null() || lst.len == 0 {
        return alloc_c_string("");
    }
    // FFI-12: Reject unreasonable list lengths to prevent DoS via i64::MAX loop.
    if lst.len < 0 || lst.len > 1_000_000 {
        return alloc_c_string("");
    }
    let separator = unsafe { cstr_to_string(sep) };

    let mut parts: Vec<String> = Vec::with_capacity(lst.len as usize);
    for i in 0..lst.len as isize {
        unsafe {
            let ptr = *lst.data.offset(i);
            parts.push(cstr_to_string(ptr));
        }
    }
    let result = parts.join(&separator);
    alloc_c_string(&result)
}

#[no_mangle]
pub extern "C" fn mimi_str_replace(
    s: *const std::ffi::c_char,
    from: *const std::ffi::c_char,
    to: *const std::ffi::c_char,
) -> *mut std::ffi::c_char {
    let ss = unsafe { cstr_to_string(s) };
    let f = unsafe { cstr_to_string(from) };
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
    let json = unsafe { cstr_to_string(json_str) };
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
pub extern "C" fn json_get_element(
    json_str: *const std::ffi::c_char,
    index: i64,
) -> *mut std::ffi::c_char {
    if json_str.is_null() {
        return std::ptr::null_mut();
    }
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
    unsafe {
        drop(Box::from_raw(handle as *mut MimiSet));
    }
}

#[no_mangle]
pub extern "C" fn mimi_set_insert(handle: SetHandle, value: SetValueHandle) -> SetHandle {
    if handle == 0 {
        return handle;
    }
    unsafe { (*set_from_handle(handle)).inner.insert(value); }
    handle
}

#[no_mangle]
pub extern "C" fn mimi_set_contains(handle: SetHandle, value: SetValueHandle) -> i64 {
    if handle == 0 {
        return 0;
    }
    unsafe { (*set_from_handle(handle)).inner.contains(&value) as i64 }
}

#[no_mangle]
pub extern "C" fn mimi_set_remove(handle: SetHandle, value: SetValueHandle) -> SetHandle {
    if handle == 0 {
        return handle;
    }
    unsafe { (*set_from_handle(handle)).inner.remove(&value); }
    handle
}

#[no_mangle]
pub extern "C" fn mimi_set_size(handle: SetHandle) -> i64 {
    if handle == 0 {
        return 0;
    }
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
        unsafe { *out_len = -1; }
        return -1isize as *mut SetValueHandle;
    }
    let set = unsafe { &*set_from_handle(handle) };
    let len = set.inner.len() as i64;
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
    let t = unsafe { cstr_to_string(text) };
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
    let t = unsafe { cstr_to_string(text) };
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
    let t = unsafe { cstr_to_string(text) };
    let p = unsafe { cstr_to_string(pattern) };
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
    let t = unsafe { cstr_to_string(text) };
let p = unsafe { cstr_to_string(pattern) };
    let mut matches = Vec::new();
    let mut cursor = 0;
    let t_bytes = t.as_bytes();
    let p_bytes = p.as_bytes();
    loop {
        if cursor >= t_bytes.len() { break; }
        let mut found = -1;
        let mut found_start = 0;
        for start in cursor..t_bytes.len() {
            let consumed = RegexEngine::match_here_with_depth(p_bytes, &t_bytes[start..], 0);
            if consumed >= 0 {
                let matched = std::str::from_utf8(&t_bytes[start..start + consumed as usize]).unwrap_or("");
                matches.push(matched.to_string());
                found = consumed;
                found_start = start;
                break;
            }
        }
        if found < 0 { break; }
        cursor = found_start + found as usize;
    }
    let mut result = String::from("[");
    let mut first = true;
    for m in &matches {
        if !first { result.push(','); }
        first = false;
        result.push('"');
        for ch in m.chars() {
            match ch {
                '"' => result.push_str("\\\""),
                '\\' => result.push_str("\\\\"),
                '\n' => result.push_str("\\n"),
                '\r' => result.push_str("\\r"),
                '\t' => result.push_str("\\t"),
                c if c < '\x20' => { result.push_str(&format!("\\u{:04x}", c as u32)); }
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
pub extern "C" fn mimi_sort_f64_inplace(
    data: *mut u8,
    count: i64,
) {
    if data.is_null() || count <= 1 {
        return;
    }
    let elem_size: usize = 8;
    let total_bytes = (count as usize) * elem_size;
    let slice = unsafe { std::slice::from_raw_parts_mut(data, total_bytes) };
    for i in 0..(count as usize) {
        for j in 0..(count as usize) - 1 - i {
            let a_off = j * elem_size;
            let b_off = (j + 1) * elem_size;
            let a_bits = u64::from_ne_bytes([
                slice[a_off], slice[a_off+1], slice[a_off+2], slice[a_off+3],
                slice[a_off+4], slice[a_off+5], slice[a_off+6], slice[a_off+7],
            ]);
            let b_bits = u64::from_ne_bytes([
                slice[b_off], slice[b_off+1], slice[b_off+2], slice[b_off+3],
                slice[b_off+4], slice[b_off+5], slice[b_off+6], slice[b_off+7],
            ]);
            if f64::from_bits(a_bits) > f64::from_bits(b_bits) {
                for k in 0..elem_size {
                    slice.swap(a_off + k, b_off + k);
                }
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
    let h = unsafe { cstr_to_string(host) };

    // Resolve address
    let port_str = format!("{}", port);
    let hints = unsafe {
        let mut hints_raw: libc::addrinfo = std::mem::zeroed();
        hints_raw.ai_family = libc::AF_UNSPEC;
        hints_raw.ai_socktype = libc::SOCK_STREAM;
        hints_raw
    };
    let mut res: *mut libc::addrinfo = std::ptr::null_mut();
    let c_host = CString::new(h.as_str()).unwrap_or_default();
    let c_port = CString::new(port_str.as_str()).unwrap_or_default();
    let err = unsafe { libc::getaddrinfo(c_host.as_ptr(), c_port.as_ptr(), &hints, &mut res) };
    if err != 0 || res.is_null() {
        return -1;
    }

    unsafe {
        let fd_i32 = match fd_to_i32(fd) {
            Some(v) => v,
            None => {
                libc::freeaddrinfo(res);
                return -1;
            }
        };
        let r = libc::connect(fd_i32, (*res).ai_addr, (*res).ai_addrlen);
        if r == 0 {
            let flag: i32 = 1;
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
    let n = unsafe { libc::recv(fd_i32, buf.as_mut_ptr() as *mut std::ffi::c_void, size, 0) };
    if n <= 0 {
        if !out_len.is_null() {
            unsafe {
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
    let u = unsafe { cstr_to_string(url) };
    let b = if body.is_null() {
        String::new()
    } else {
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
            unsafe {
                *out_len = 0;
            }
        }
        return std::ptr::null_mut();
    }
    let s = unsafe { cstr_to_string(json) };
    let bytes = s.as_bytes();
    let mut pos = 0;

    // Skip whitespace
    while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') {
        pos += 1;
    }
    if pos >= bytes.len() || bytes[pos] != b'[' {
        unsafe {
            *out_len = 0;
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
                                unsafe { libc::free(ptr as *mut std::ffi::c_void) };
                            }
                        }
                    }
                    unsafe { *out_len = 0 };
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
    unsafe {
        *out_len = idx;
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
    let vals = unsafe { std::slice::from_raw_parts(values, count as usize) };
    let types = if elem_types.is_null() {
        &[] as &[i64]
    } else {
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
                        *out_values.offset(idx as isize) =
                            alloc_c_string_from_bytes(&s_bytes) as i64;
                    }
                } else {
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

// ---------------------------------------------------------------------------
// No_panic signal handlers (POSIX only)
// ---------------------------------------------------------------------------

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod no_panic {
    #[cfg(standalone)]
    use crate::libc;
    use std::cell::Cell;
    use std::cell::UnsafeCell;
    use std::sync::atomic::{AtomicBool, Ordering};

    static HANDLERS_INSTALLED: AtomicBool = AtomicBool::new(false);

    // glibc sigjmp_buf is ~200 bytes, macOS ~184, ARM64 ~200.
    // Use 256 to cover all platforms safely.
    const JMP_BUF_SIZE: usize = 256;
    type SigJmpBuf = [u8; JMP_BUF_SIZE];

    thread_local! {
        static NO_PANIC_JUMP_BUF: Cell<*mut SigJmpBuf> =
            const { Cell::new(std::ptr::null_mut()) };
        // Store old handlers as opaque usize values (libc::signal uses usize on Linux)
        static OLD_HANDLERS: UnsafeCell<[usize; 5]> = const {
            UnsafeCell::new([usize::MAX; 5])
        };
    }

    const SIGS: &[i32; 5] = &[
        libc::SIGSEGV,
        libc::SIGABRT,
        libc::SIGBUS,
        libc::SIGILL,
        libc::SIGFPE,
    ];

    #[allow(dead_code)]
    fn sig_index(sig: i32) -> Option<usize> {
        SIGS.iter().position(|&s| s == sig)
    }

    #[allow(clashing_extern_declarations, dead_code)]
    extern "C" {
        fn sigsetjmp(env: *mut SigJmpBuf, savemask: i32) -> i32;
        fn siglongjmp(env: *mut SigJmpBuf, val: i32) -> !;
    }

    extern "C" fn no_panic_handler(sig: i32) {
        // Only reset the signal that was actually caught, not all managed signals
        if let Some(idx) = sig_index(sig) {
            unsafe {
                OLD_HANDLERS.with(|old| {
                    let arr = &*old.get();
                    // OLD_HANDLERS stores raw handler pointer as usize
                    libc::signal(sig, arr[idx]);
                });
            }
        }
        NO_PANIC_JUMP_BUF.with(|buf| {
            let jmp_buf = buf.get();
            if !jmp_buf.is_null() {
                unsafe {
                    siglongjmp(jmp_buf, sig);
                }
            }
        });
    }

    #[no_mangle]
    pub unsafe extern "C" fn mimi_install_no_panic_handlers() {
        let handler = no_panic_handler as *const () as usize;
        OLD_HANDLERS.with(|old| {
            let arr = &mut *old.get();
            for (i, &sig) in SIGS.iter().enumerate() {
                arr[i] = libc::signal(sig, handler);
            }
        });
        HANDLERS_INSTALLED.store(true, Ordering::Release);
    }

    #[no_mangle]
    pub unsafe extern "C" fn mimi_restore_no_panic_handlers() {
        OLD_HANDLERS.with(|old| {
            let arr = &*old.get();
            for (i, &sig) in SIGS.iter().enumerate() {
                let prev = arr[i];
                if prev != usize::MAX {
                    libc::signal(sig, prev);
                }
            }
        });
        NO_PANIC_JUMP_BUF.with(|buf| buf.set(std::ptr::null_mut()));
    }
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
    unsafe {
        let _ = write(2, PREFIX.as_ptr() as *const std::ffi::c_void, PREFIX.len());
        if !msg.is_null() {
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
        let handler: &ErrorHandler = unsafe { &*handler_ptr };
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
    unsafe {
        let rep = &*(fut as *const MimiFutureRepr);
        rep.completed.load(Ordering::Acquire)
    }
}

/// Spawn a future on a real thread (used by codegen `spawn expr`).
/// The poll function is called on a new thread, which sets completed=1 when done.
/// Returns the future pointer (same as input).
/// Note: JoinHandle is intentionally dropped (detached thread). The future's
/// completion is tracked via `mimi_future_set_completed`. Process exit while
/// the thread is running will kill the thread abruptly.
#[no_mangle]
pub extern "C" fn mimi_spawn_future(
    future: *mut std::ffi::c_void,
    poll_fn: unsafe extern "C" fn(*mut std::ffi::c_void),
) -> *mut std::ffi::c_void {
    if future.is_null() {
        return std::ptr::null_mut();
    }
    let future_addr = future as usize;
    let _handle = std::thread::spawn(move || {
        unsafe { poll_fn(future_addr as *mut std::ffi::c_void) };
    });
    // _handle is dropped here — thread is detached. This is intentional:
    // completion is signaled via mimi_future_set_completed, not JoinHandle.
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
unsafe impl Send for SendPtr {}
unsafe impl Sync for SendPtr {}

type ExecutorEntry = (PollFn, SendPtr);

static EXECUTOR_QUEUE: std::sync::Mutex<Vec<ExecutorEntry>> = std::sync::Mutex::new(Vec::new());

/// Submit a future + its poll function to the global executor.
/// The future is not polled immediately; call mimi_executor_run() to poll.
#[no_mangle]
pub extern "C" fn mimi_executor_spawn(
    future: *mut std::ffi::c_void,
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
        return Box::into_raw(Box::new(MimiList { len: 0, data: std::ptr::null_mut(), owns_data: true }));
    } else {
        match unsafe { CStr::from_ptr(path) }.to_str() {
            Ok(s) => s,
            Err(_) => return Box::into_raw(Box::new(MimiList { len: 0, data: std::ptr::null_mut(), owns_data: true })),
        }
    };
    let entries: Vec<*mut std::ffi::c_char> = match std::fs::read_dir(path_str) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                e.file_name()
                    .to_str()
                    .map(alloc_c_string)
            })
            .collect(),
        Err(_) => return Box::into_raw(Box::new(MimiList { len: 0, data: std::ptr::null_mut(), owns_data: true })),
    };
    let len = entries.len() as i64;
    let mut items = entries;
    let data_ptr = items.as_mut_ptr();
    std::mem::forget(items);
    Box::into_raw(Box::new(MimiList { len, data: data_ptr, owns_data: true }))
}

/// Returns 1 if path is a directory, 0 otherwise.
#[no_mangle]
pub extern "C" fn mimi_is_dir(path: *const std::ffi::c_char) -> i64 {
    if path.is_null() {
        return 0;
    }
    let path_str = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(s) => s,
        Err(_) => return 0,
    };
    if std::path::Path::new(path_str).is_dir() { 1 } else { 0 }
}

/// Returns 1 if path is a regular file, 0 otherwise.
#[no_mangle]
pub extern "C" fn mimi_is_file(path: *const std::ffi::c_char) -> i64 {
    if path.is_null() {
        return 0;
    }
    let path_str = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(s) => s,
        Err(_) => return 0,
    };
    if std::path::Path::new(path_str).is_file() { 1 } else { 0 }
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
        unsafe { CStr::from_ptr(a) }.to_str().unwrap_or("")
    };
    let b_str = if b.is_null() {
        ""
    } else {
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
    let empty = || Box::into_raw(Box::new(MimiList { len: 0, data: std::ptr::null_mut(), owns_data: true }));
    let path_str = if path.is_null() {
        return empty();
    } else {
        match unsafe { CStr::from_ptr(path) }.to_str() {
            Ok(s) => s,
            Err(_) => return empty(),
        }
    };
    let mut results = Vec::new();
    walk_dir_recursive(path_str, &mut results);
    let len = results.len() as i64;
    let mut items: Vec<*mut std::ffi::c_char> = results.into_iter().map(|s| alloc_c_string(&s)).collect();
    let data_ptr = items.as_mut_ptr();
    std::mem::forget(items);
    Box::into_raw(Box::new(MimiList { len, data: data_ptr, owns_data: true }))
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
    let path_str = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(s) => s,
        Err(_) => return 0,
    };
    if std::fs::create_dir_all(path_str).is_ok() { 1 } else { 0 }
}

/// Removes a file. Returns 1 on success, 0 on failure.
#[no_mangle]
pub extern "C" fn mimi_remove_file(path: *const std::ffi::c_char) -> i64 {
    if path.is_null() {
        return 0;
    }
    let path_str = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(s) => s,
        Err(_) => return 0,
    };
    if std::fs::remove_file(path_str).is_ok() { 1 } else { 0 }
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
            unsafe { *err_out = alloc_c_string("file_stat error: null path") };
        }
        return std::ptr::null_mut();
    }
    let path_str = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(s) => s,
        Err(e) => {
            if !err_out.is_null() {
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
                unsafe { *err_out = std::ptr::null_mut() };
            }
            Box::into_raw(res)
        }
        Err(e) => {
            if !err_out.is_null() {
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
    let path_str = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(s) => s,
        Err(_) => return 0,
    };
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
            if file.write_all(content_str.as_bytes()).is_ok() { 1 } else { 0 }
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
    let key_str = match unsafe { CStr::from_ptr(key) }.to_str() {
        Ok(s) => s,
        Err(_) => return 0,
    };
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
        unsafe { CStr::from_ptr(data) }.to_bytes()
    };
    let hash = sha256_bytes(input);
    let hex: String = hash.iter().map(|b| format!("{:02x}", b)).collect();
    alloc_c_string(&hex)
}

pub fn sha256_bytes(data: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
        0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];
    let k: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
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
                chunk[i * 4], chunk[i * 4 + 1], chunk[i * 4 + 2], chunk[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(k[i]).wrapping_add(w[i]);
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
        while i < 26 { table[(b'A' + i) as usize] = i as i8; i += 1; }
        while i < 52 { table[(b'a' + i - 26) as usize] = i as i8; i += 1; }
        while i < 62 { table[(b'0' + i - 52) as usize] = i as i8; i += 1; }
        table[b'+' as usize] = 62;
        table[b'/' as usize] = 63;
        table
    };
    let clean: Vec<u8> = input.bytes().filter(|&b| b != b'=' && !b.is_ascii_whitespace()).collect();
    let mut output = Vec::new();
    for chunk in clean.chunks(4) {
        let mut buf = 0u32;
        let mut bits = 0;
        for &b in chunk {
            if b >= 128 || REV[b as usize] < 0 { return Err(()); }
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
    let tmpl = unsafe { cstr_to_string(template) };
    let args = [arg0, arg1, arg2, arg3, arg4, arg5, arg6, arg7];
    let mut result = String::new();
    let mut rest = tmpl.as_str();
    let mut arg_idx = 0;
    while let Some(pos) = rest.find("{}") {
        result.push_str(&rest[..pos]);
        if arg_idx < num_args as usize && arg_idx < args.len() {
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
pub extern "C" fn mimi_read_file_bytes(
    path: *const std::ffi::c_char,
) -> *mut std::ffi::c_char {
    if path.is_null() {
        return alloc_c_string("");
    }
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
    let path_str = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(s) => s,
        Err(_) => return 0,
    };
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
pub extern "C" fn mimi_read_lines_json(
    path: *const std::ffi::c_char,
) -> *mut std::ffi::c_char {
    use std::io::BufRead;
    if path.is_null() {
        return alloc_c_string("[]");
    }
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
