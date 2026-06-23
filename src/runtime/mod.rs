// Mimi language runtime — pure Rust implementation.
//
// This module provides all runtime symbols needed by LLVM-codegened Mimi programs,
// replacing the previous C implementation (mimi_runtime.c). Every function is
// `#[no_mangle] pub extern "C"` so it can be linked from generated machine code.
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
            sockfd: i32, level: i32, optname: i32,
            optval: *const c_void, optlen: socklen_t,
        ) -> i32;
        pub fn bind(sockfd: i32, addr: *const sockaddr, addrlen: socklen_t) -> i32;
        pub fn listen(sockfd: i32, backlog: i32) -> i32;
        pub fn accept(sockfd: i32, addr: *mut sockaddr, addrlen: *mut socklen_t) -> i32;
        pub fn connect(sockfd: i32, addr: *const sockaddr, addrlen: socklen_t) -> i32;
        pub fn send(sockfd: i32, buf: *const c_void, len: usize, flags: i32) -> isize;
        pub fn recv(sockfd: i32, buf: *mut c_void, len: usize, flags: i32) -> isize;
        pub fn close(fd: i32) -> i32;
        pub fn getaddrinfo(
            node: *const i8, service: *const i8,
            hints: *const addrinfo, res: *mut *mut addrinfo,
        ) -> i32;
        pub fn freeaddrinfo(res: *mut addrinfo);
        pub fn signal(signum: i32, handler: usize) -> usize;
    }
}

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::ffi::{CStr, CString};
use std::sync::Mutex;

// Re-export types used by FFI tests and codegen
// Must match the C layouts exactly.
#[repr(C)]
pub struct MimiList {
    len: i64,
    data: *mut *mut std::ffi::c_char,
}

pub type ValueHandle = usize;
pub type MapHandle = usize;

// ---------------------------------------------------------------------------
// Integer math
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn __mimi_pow_i64(base: i64, exp: i64) -> i64 {
    if exp < 0 { return 0; }
    if exp == 0 { return 1; }
    let mut result: i64 = 1;
    let mut b: i64 = base;
    let mut e: i64 = exp;
    while e > 0 {
        if (e & 1) != 0 {
            if b != 0 && result > (i64::MAX / b) { return 0; }
            result = result.wrapping_mul(b);
        }
        e >>= 1;
        if e > 0 {
            if b != 0 && b > (i64::MAX / b) { return 0; }
            b = b.wrapping_mul(b);
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Reference counting (atomic)
// ---------------------------------------------------------------------------
// Layout: [AtomicI64 strong | AtomicI64 weak | user data ...]
// Returns pointer to user data (right after refcount header).

#[repr(C)]
struct RcHeader {
    strong: AtomicI64,
    weak: AtomicI64,
}

unsafe fn rc_header_from_ptr<'a>(ptr: *mut std::ffi::c_void) -> &'a RcHeader {
    let hdr = (ptr as *mut RcHeader).sub(1);
    &*hdr
}

unsafe fn rc_header_from_ptr_mut(ptr: *mut std::ffi::c_void) -> &'static mut RcHeader {
    let hdr = (ptr as *mut RcHeader).sub(1);
    &mut *hdr
}

#[no_mangle]
pub extern "C" fn mimi_rc_alloc(size: i64) -> *mut std::ffi::c_void {
    let total = std::alloc::Layout::new::<RcHeader>()
        .extend(std::alloc::Layout::array::<u8>(size as usize).expect("invalid layout"))
        .expect("layout extension failed")
        .0
        .pad_to_align();
    let ptr = unsafe { std::alloc::alloc(total) };
    if ptr.is_null() { return std::ptr::null_mut(); }
    let hdr = ptr as *mut RcHeader;
    unsafe {
        (*hdr).strong = AtomicI64::new(1);
        (*hdr).weak = AtomicI64::new(0);
    }
    unsafe { (hdr.add(1)) as *mut std::ffi::c_void }
}

#[no_mangle]
pub extern "C" fn mimi_rc_retain(ptr: *mut std::ffi::c_void) {
    if ptr.is_null() { return; }
    let hdr = unsafe { rc_header_from_ptr(ptr) };
    hdr.strong.fetch_add(1, Ordering::Relaxed);
}

#[no_mangle]
pub extern "C" fn mimi_rc_release(ptr: *mut std::ffi::c_void) {
    if ptr.is_null() { return; }
    let hdr = unsafe { rc_header_from_ptr(ptr) };
    if hdr.strong.fetch_sub(1, Ordering::Release) == 1 {
        std::sync::atomic::fence(Ordering::Acquire);
        if hdr.weak.load(Ordering::Relaxed) == 0 {
            let hdr_mut = unsafe { rc_header_from_ptr_mut(ptr) };
            let layout = std::alloc::Layout::new::<RcHeader>()
                .extend(std::alloc::Layout::array::<u8>(0).expect("invalid layout"))
                .expect("layout extension failed")
                .0
                .pad_to_align();
            unsafe { std::alloc::dealloc(hdr_mut as *mut RcHeader as *mut u8, layout); }
        }
    }
}

#[no_mangle]
pub extern "C" fn mimi_rc_weak_retain(ptr: *mut std::ffi::c_void) {
    if ptr.is_null() { return; }
    let hdr = unsafe { rc_header_from_ptr(ptr) };
    hdr.weak.fetch_add(1, Ordering::Relaxed);
}

#[no_mangle]
pub extern "C" fn mimi_rc_weak_release(ptr: *mut std::ffi::c_void) {
    if ptr.is_null() { return; }
    let hdr = unsafe { rc_header_from_ptr(ptr) };
    if hdr.weak.fetch_sub(1, Ordering::Release) == 1 {
        std::sync::atomic::fence(Ordering::Acquire);
        if hdr.strong.load(Ordering::Relaxed) <= 0 {
            let hdr_mut = unsafe { rc_header_from_ptr_mut(ptr) };
            let layout = std::alloc::Layout::new::<RcHeader>()
                .extend(std::alloc::Layout::array::<u8>(0).expect("invalid layout"))
                .expect("layout extension failed")
                .0
                .pad_to_align();
            unsafe { std::alloc::dealloc(hdr_mut as *mut RcHeader as *mut u8, layout); }
        }
    }
}

#[no_mangle]
pub extern "C" fn mimi_rc_upgrade(ptr: *mut std::ffi::c_void) -> *mut std::ffi::c_void {
    if ptr.is_null() { return std::ptr::null_mut(); }
    let hdr = unsafe { rc_header_from_ptr(ptr) };
    let mut s = hdr.strong.load(Ordering::Relaxed);
    loop {
        if s == 0 { return std::ptr::null_mut(); }
        match hdr.strong.compare_exchange_weak(s, s + 1, Ordering::Acquire, Ordering::Relaxed) {
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

unsafe fn map_from_handle(handle: MapHandle) -> &'static mut MimiMap {
    &mut *(handle as *mut MimiMap)
}

#[no_mangle]
pub extern "C" fn mimi_map_new() -> MapHandle {
    let map = Box::new(MimiMap { inner: HashMap::new() });
    Box::into_raw(map) as MapHandle
}

#[no_mangle]
pub extern "C" fn mimi_map_destroy(handle: MapHandle) {
    if handle == 0 { return; }
    unsafe { drop(Box::from_raw(handle as *mut MimiMap)); }
}

#[no_mangle]
pub extern "C" fn mimi_map_size(handle: MapHandle) -> i64 {
    if handle == 0 { return 0; }
    unsafe { map_from_handle(handle).inner.len() as i64 }
}

fn cstr_to_str<'a>(ptr: *const std::ffi::c_char) -> &'a str {
    if ptr.is_null() { return ""; }
    unsafe { CStr::from_ptr(ptr) }.to_str().unwrap_or("")
}

#[no_mangle]
pub extern "C" fn mimi_map_has_key(handle: MapHandle, key: *const std::ffi::c_char) -> i32 {
    if handle == 0 || key.is_null() { return 0; }
    let s = cstr_to_str(key);
    unsafe { map_from_handle(handle).inner.contains_key(s) as i32 }
}

#[no_mangle]
pub extern "C" fn mimi_map_get(handle: MapHandle, key: *const std::ffi::c_char) -> ValueHandle {
    if handle == 0 || key.is_null() { return 0; }
    let s = cstr_to_str(key);
    unsafe {
        map_from_handle(handle).inner.get(s).copied().unwrap_or(0)
    }
}

#[no_mangle]
pub extern "C" fn mimi_map_set(handle: MapHandle, key: *const std::ffi::c_char, value: ValueHandle) {
    if handle == 0 || key.is_null() { return; }
    let s = cstr_to_str(key);
    unsafe { map_from_handle(handle).inner.insert(s.to_string(), value); }
}

#[no_mangle]
pub extern "C" fn mimi_map_remove(handle: MapHandle, key: *const std::ffi::c_char) -> i32 {
    if handle == 0 || key.is_null() { return 0; }
    let s = cstr_to_str(key);
    unsafe { map_from_handle(handle).inner.remove(s).is_some() as i32 }
}

#[no_mangle]
pub extern "C" fn mimi_map_from_list(
    keys: *mut ValueHandle,
    values: *mut ValueHandle,
    n: i64,
) -> MapHandle {
    let handle = mimi_map_new();
    if handle == 0 || keys.is_null() || values.is_null() || n == 0 { return handle; }
    for i in 0..n {
        unsafe {
            let key_handle = *keys.add(i as usize);
            let val_handle = *values.add(i as usize);
            let key_str = key_handle as *const std::ffi::c_char;
            if !key_str.is_null() {
                let s = CStr::from_ptr(key_str).to_str().unwrap_or("");
                map_from_handle(handle).inner.insert(s.to_string(), val_handle);
            }
        }
    }
    handle
}

fn mimi_map_collect(handle: MapHandle, collect_values: bool) -> *mut MimiList {
    if handle == 0 {
        let list = Box::new(MimiList { len: 0, data: std::ptr::null_mut() });
        return Box::into_raw(list);
    }
    let map = unsafe { map_from_handle(handle) };
    let len = map.inner.len() as i64;
    if len == 0 {
        let list = Box::new(MimiList { len: 0, data: std::ptr::null_mut() });
        return Box::into_raw(list);
    }

    let mut items: Vec<*mut std::ffi::c_char> = Vec::with_capacity(len as usize);
    for (k, v) in &map.inner {
        if collect_values {
            // ValueHandle is stored as-is (it's an opaque integer)
            let val_ptr = *v as *mut std::ffi::c_char;
            items.push(val_ptr);
        } else {
            let c_str = CString::new(k.as_str()).unwrap_or_default();
            items.push(c_str.into_raw());
        }
    }

    let data_ptr = items.as_mut_ptr();
    std::mem::forget(items);
    let list = Box::new(MimiList { len, data: data_ptr });
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
    if ptr.is_null() { return String::new(); }
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
    CString::new(result).unwrap_or_default().into_raw()
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
    let mut c_strings: Vec<*mut std::ffi::c_char> = parts
        .into_iter()
        .map(|p| CString::new(p).unwrap_or_default().into_raw())
        .collect();
    let data_ptr = c_strings.as_mut_ptr();
    std::mem::forget(c_strings);

    let list = Box::new(MimiList { len, data: data_ptr });
    Box::into_raw(list)
}

#[no_mangle]
pub extern "C" fn mimi_str_join(
    list: *const MimiList,
    sep: *const std::ffi::c_char,
) -> *mut std::ffi::c_char {
    if list.is_null() {
        return CString::new("").unwrap_or_default().into_raw();
    }
    let lst = unsafe { &*list };
    if lst.data.is_null() || lst.len == 0 {
        return CString::new("").unwrap_or_default().into_raw();
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
    CString::new(result).unwrap_or_default().into_raw()
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
        return CString::new(ss).unwrap_or_default().into_raw();
    }
    let result = ss.replace(&f, &t);
    CString::new(result).unwrap_or_default().into_raw()
}

// ---------------------------------------------------------------------------
// Try/exit (? operator)
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn mimi_try_exit(payload: i64) -> ! {
    eprintln!("Error: Result::Err({})", payload);
    std::process::exit(1);
}

#[no_mangle]
pub extern "C" fn mimi_try_exit_str(str: *const std::ffi::c_char, _len: i64) -> ! {
    let msg = unsafe { cstr_to_string(str) };
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
        Mutex::new(CliArgs { argc: 0, argv: Vec::new() })
    });
}

#[no_mangle]
pub extern "C" fn mimi_args_init(argc: i32, argv: *mut *mut std::ffi::c_char) {
    init_cli_args();
    let args_mutex = CLI_ARGS.get().expect("CLI_ARGS not initialized");
    let mut args = args_mutex.lock().expect("lock poisoned");
    args.argc = argc;
    args.argv.clear();
    if !argv.is_null() && argc > 0 {
        for i in 0..argc as isize {
            unsafe {
                args.argv.push(*argv.offset(i) as usize);
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn mimi_getenv(name: *const std::ffi::c_char) -> *mut std::ffi::c_char {
    let n = unsafe { cstr_to_string(name) };
    match std::env::var(&n) {
        Ok(val) => CString::new(val).unwrap_or_default().into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "C" fn mimi_args_count() -> i64 {
    init_cli_args();
    let args_mutex = CLI_ARGS.get().expect("CLI_ARGS not initialized");
    let args = args_mutex.lock().expect("lock poisoned");
    if args.argc <= 1 { return 0; }
    (args.argc - 1) as i64
}

#[no_mangle]
pub extern "C" fn mimi_args_get(i: i64) -> *mut std::ffi::c_char {
    init_cli_args();
    let args_mutex = CLI_ARGS.get().expect("CLI_ARGS not initialized");
    let args = args_mutex.lock().expect("lock poisoned");
    if i < 0 || i >= (args.argc - 1) as i64 { return std::ptr::null_mut(); }
    let idx = (i + 1) as usize; // +1 to skip program name
    args.argv.get(idx).copied().map(|p| p as *mut std::ffi::c_char).unwrap_or(std::ptr::null_mut())
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
        Self { p: input.as_bytes(), pos: 0, depth: 0 }
    }

    fn peek(&self) -> u8 {
        if self.pos < self.p.len() { self.p[self.pos] } else { 0 }
    }

    fn advance(&mut self) {
        if self.pos < self.p.len() { self.pos += 1; }
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
        if self.pos >= self.p.len() { return None; }
        self.depth += 1;
        if self.depth > JSON_MAX_DEPTH { return None; }

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
        if self.peek() != b'"' { return None; }
        self.advance(); // skip "
        let _start = self.pos;
        let mut result = String::new();
        let mut esc = false;
        loop {
            if self.pos >= self.p.len() { return None; }
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
                        if self.pos + 4 >= self.p.len() { return None; }
                        let hex_str = &self.p[self.pos+1..self.pos+5];
                        let hex = std::str::from_utf8(hex_str).ok()?;
                        let cp = u32::from_str_radix(hex, 16).ok()?;
                        if let Some(ch) = char::from_u32(cp) {
                            result.push(ch);
                        }
                        self.pos += 4;
                    }
                    _ => { result.push(c as char); }
                }
                esc = false;
                self.pos += 1;
                continue;
            }
            if c == b'\\' { esc = true; self.pos += 1; continue; }
            if c == b'"' { self.pos += 1; return Some(result); }
            result.push(c as char);
            self.pos += 1;
        }
    }

    fn parse_number(&mut self) -> Option<String> {
        let start = self.pos;
        if self.peek() == b'-' { self.advance(); }
        if self.pos >= self.p.len() || !self.peek().is_ascii_digit() { return None; }
        while self.pos < self.p.len() && self.p[self.pos].is_ascii_digit() { self.advance(); }

        let mut is_float = false;
        if self.pos < self.p.len() && self.p[self.pos] == b'.' {
            is_float = true;
            self.advance();
            let mut has_digits = false;
            while self.pos < self.p.len() && self.p[self.pos].is_ascii_digit() {
                has_digits = true;
                self.advance();
            }
            if !has_digits { return None; }
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
            if !has_digits { return None; }
        }

        let s = std::str::from_utf8(&self.p[start..self.pos]).ok()?;
        if is_float {
            // Format float: trim trailing zeros
            let val: f64 = s.parse().ok()?;
            let mut formatted = format!("{}", val);
            if formatted.contains('.') {
                formatted = formatted.trim_end_matches('0').trim_end_matches('.').to_string();
            }
            Some(formatted)
        } else {
            Some(s.to_string())
        }
    }

    fn parse_literal(&mut self, expected: &str, value: &str) -> Option<String> {
        let bytes = expected.as_bytes();
        if self.pos + bytes.len() > self.p.len() { return None; }
        if &self.p[self.pos..self.pos + bytes.len()] == bytes {
            self.pos += bytes.len();
            Some(value.to_string())
        } else {
            None
        }
    }

    fn parse_object(&mut self) -> Option<String> {
        if self.peek() != b'{' { return None; }
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
                        if self.pos >= self.p.len() { return None; }
                        if self.p[self.pos] == b'\\' { self.pos += 2; continue; }
                        if self.p[self.pos] == b'"' { break; }
                        self.pos += 1;
                    }
                }
                _ => {}
            }
            if depth > 0 { self.pos += 1; }
        }
        if depth != 0 { return None; }
        let s = std::str::from_utf8(&self.p[start..self.pos]).ok()?;
        self.pos += 1; // skip }
        Some(format!("{{{}}}", s))
    }

    fn parse_array(&mut self) -> Option<String> {
        if self.peek() != b'[' { return None; }
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
                        if self.pos >= self.p.len() { return None; }
                        if self.p[self.pos] == b'\\' { self.pos += 2; continue; }
                        if self.p[self.pos] == b'"' { break; }
                        self.pos += 1;
                    }
                }
                _ => {}
            }
            if depth > 0 { self.pos += 1; }
        }
        if depth != 0 { return None; }
        let s = std::str::from_utf8(&self.p[start..self.pos]).ok()?;
        self.pos += 1; // skip ]
        Some(format!("[{}]", s))
    }

    fn parse_full(&mut self) -> Option<String> {
        let val = self.parse_value()?;
        self.skip_ws();
        if self.pos != self.p.len() { return None; } // trailing garbage
        Some(val)
    }

    fn is_valid(&mut self) -> bool {
        self.parse_full().is_some()
    }
}

#[no_mangle]
pub extern "C" fn mimi_from_json(json_str: *const std::ffi::c_char) -> *mut std::ffi::c_void {
    if json_str.is_null() { return std::ptr::null_mut(); }
    let s = unsafe { cstr_to_string(json_str) };
    let mut parser = JsonParser::new(&s);
    match parser.parse_full() {
        Some(val) => CString::new(val).unwrap_or_default().into_raw() as *mut std::ffi::c_void,
        None => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "C" fn mimi_is_valid_json(json_str: *const std::ffi::c_char) -> i64 {
    if json_str.is_null() { return 0; }
    let s = unsafe { cstr_to_string(json_str) };
    let mut parser = JsonParser::new(&s);
    parser.is_valid() as i64
}

fn json_get_inner(json_str: *const std::ffi::c_char, key: *const std::ffi::c_char) -> Option<String> {
    if json_str.is_null() || key.is_null() { return None; }
    let json = unsafe { cstr_to_string(json_str) };
    let k = unsafe { cstr_to_string(key) };
    let bytes = json.as_bytes();
    let mut pos = 0;

    // Skip whitespace
    while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') { pos += 1; }
    if pos >= bytes.len() || bytes[pos] != b'{' { return None; }
    pos += 1;

    loop {
        if pos >= bytes.len() || bytes[pos] == b'}' { return None; }
        while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') { pos += 1; }

        // Parse key string
        if bytes[pos] != b'"' { return None; }
        pos += 1;
        let mut key_buf = String::new();
        loop {
            if pos >= bytes.len() { return None; }
            if bytes[pos] == b'\\' { pos += 2; key_buf.push('?'); continue; }
            if bytes[pos] == b'"' { pos += 1; break; }
            key_buf.push(bytes[pos] as char);
            pos += 1;
        }

        while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') { pos += 1; }
        if pos >= bytes.len() || bytes[pos] != b':' { return None; }
        pos += 1;
        while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') { pos += 1; }

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

        while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') { pos += 1; }
        if pos >= bytes.len() { return None; }
        if bytes[pos] == b',' { pos += 1; }
    }
}

#[no_mangle]
pub extern "C" fn json_get_string(
    json_str: *const std::ffi::c_char,
    key: *const std::ffi::c_char,
) -> *mut std::ffi::c_char {
    match json_get_inner(json_str, key) {
        Some(val) => CString::new(val).unwrap_or_default().into_raw(),
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
    if json_str.is_null() { return std::ptr::null_mut(); }
    let json = unsafe { cstr_to_string(json_str) };
    let bytes = json.as_bytes();
    let mut pos = 0;

    while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') { pos += 1; }
    if pos >= bytes.len() || bytes[pos] != b'[' { return std::ptr::null_mut(); }
    pos += 1;

    let mut idx: i64 = 0;
    loop {
        if pos >= bytes.len() || bytes[pos] == b']' { return std::ptr::null_mut(); }
        while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') { pos += 1; }

        if idx == index {
            let val_start = pos;
            let mut parser = JsonParser::new(&json[val_start..]);
            return match parser.parse_value() {
                Some(val) => CString::new(val).unwrap_or_default().into_raw(),
                None => std::ptr::null_mut(),
            };
        }

        let val_start = pos;
        let mut dummy_parser = JsonParser::new(&json[val_start..]);
        if dummy_parser.parse_value().is_none() { return std::ptr::null_mut(); }
        pos = val_start + dummy_parser.pos;

        while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') { pos += 1; }
        if pos >= bytes.len() { return std::ptr::null_mut(); }
        if bytes[pos] == b',' { pos += 1; }
        idx += 1;
    }
}

// ---------------------------------------------------------------------------
// Regex (simple recursive backtracking engine, self-contained)
// ---------------------------------------------------------------------------

struct RegexEngine;

impl RegexEngine {
    fn match_pattern(text: &str, pattern: &str) -> bool {
        let text_bytes = text.as_bytes();
        let pat_bytes = pattern.as_bytes();
        let anchored = !pat_bytes.is_empty() && pat_bytes[0] == b'^';

        for start in 0..=text_bytes.len() {
            let result = Self::match_here(pat_bytes, &text_bytes[start..]);
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
            let consumed = Self::match_here(pat_bytes, &text_bytes[start..]);
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
            if cursor >= text_bytes.len() { break; }
            let mut best_pos = text_bytes.len() + 1;
            let mut best_len = 0;
            for start in cursor..text_bytes.len() {
                let consumed = Self::match_here(pat_bytes, &text_bytes[start..]);
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
    fn match_here(pattern: &[u8], text: &[u8]) -> i32 {
        let mut pi = 0;
        let mut ti = 0;
        let plen = pattern.len();
        let tlen = text.len();

        // Skip leading ^
        if pi < plen && pattern[pi] == b'^' { pi += 1; }

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
            if elem_end == pi { return -1; }

            // Check for quantifier
            let has_star = elem_end < plen && pattern[elem_end] == b'*';
            let has_plus = elem_end < plen && pattern[elem_end] == b'+';
            let after_quant = if has_star || has_plus { elem_end + 1 } else { elem_end };

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
                    let r = Self::match_here_from(pattern, after_quant, text, ti + count);
                    if r >= 0 {
                        ti = ti + count + r as usize;
                        matched = true;
                        break;
                    }
                }
                if !matched { return -1; }
                pi = plen; // after_quant is already consumed via recursive call
                continue;
            }

            if ti >= tlen { return -1; }
            if !Self::elem_match(pattern, &mut pi, text[ti], elem_is_class) { return -1; }
            ti += 1;
        }
    }

    fn match_here_from(pattern: &[u8], start: usize, text: &[u8], ti: usize) -> i32 {
        // Reconstruct a pattern slice from start and check match from position ti
        let sub_pat = &pattern[start..];
        let sub_text = &text[ti..];
        Self::match_here(sub_pat, sub_text)
    }

    /// Parse pattern element starting at pi, return (end_pos, is_class).
    fn parse_element(pattern: &[u8], pi: usize) -> (usize, bool) {
        if pi >= pattern.len() { return (pi, false); }
        match pattern[pi] {
            b'\\' => (pi + 2, false),
            b'[' => {
                let mut ep = pi + 1;
                if ep < pattern.len() && pattern[ep] == b'^' { ep += 1; }
                while ep < pattern.len() && pattern[ep] != b']' {
                    if pattern[ep] == b'\\' && ep + 1 < pattern.len() { ep += 2; }
                    else { ep += 1; }
                }
                if ep < pattern.len() { ep += 1; } // skip ]
                (ep, true)
            }
            _ => (pi + 1, false),
        }
    }

    fn elem_match_in_class(class: &[u8], c: u8, start: usize) -> (bool, usize) {
        let mut pos = start;
        let neg = pos < class.len() && class[pos] == b'^';
        if neg { pos += 1; }

        let mut matched = false;
        while pos < class.len() && class[pos] != b']' {
            if pos + 2 < class.len() && class[pos + 1] == b'-' && class[pos + 2] != b']' {
                if c >= class[pos] && c <= class[pos + 2] { matched = true; }
                pos += 3;
            } else {
                if c == class[pos] { matched = true; }
                pos += 1;
            }
        }
        // Advance to end of class
        while pos < class.len() && class[pos] != b']' { pos += 1; }
        if pos < class.len() { pos += 1; } // skip ]

        if neg { (!matched, pos) } else { (matched, pos) }
    }

    /// Check if pattern element at pi matches character c. Advances pi past element.
    fn elem_match(pattern: &[u8], pi: &mut usize, c: u8, is_class: bool) -> bool {
        if *pi >= pattern.len() { return false; }

        if is_class {
            // [...] class
            let class_start = *pi + 1; // skip [
            let (matched, end) = Self::elem_match_in_class(pattern, c, class_start);
            *pi = end;
            return matched;
        }

        match pattern[*pi] {
            b'\\' => {
                if *pi + 1 >= pattern.len() { return false; }
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
    if text.is_null() || pattern.is_null() { return 0; }
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
        return CString::new("").unwrap_or_default().into_raw();
    }
    let t = unsafe { cstr_to_string(text) };
    let p = unsafe { cstr_to_string(pattern) };
    match RegexEngine::find_match(&t, &p) {
        Some((start, end)) => {
            let matched = &t[start..end];
            CString::new(matched).unwrap_or_default().into_raw()
        }
        None => CString::new("").unwrap_or_default().into_raw(),
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
    CString::new(result).unwrap_or_default().into_raw()
}

// ---------------------------------------------------------------------------
// Network / Socket
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn mimi_socket(domain: i64, type_: i64, protocol: i64) -> i64 {
    // We'll use libc calls directly.
    unsafe {
        let fd = libc::socket(domain as i32, type_ as i32, protocol as i32);
        if fd >= 0 {
            let reuse: i32 = 1;
            libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_REUSEADDR,
                             &reuse as *const _ as *const std::ffi::c_void,
                             std::mem::size_of::<i32>() as libc::socklen_t);
        }
        fd as i64
    }
}

#[no_mangle]
pub extern "C" fn mimi_connect(
    fd: i64,
    host: *const std::ffi::c_char,
    port: i64,
) -> i64 {
    if host.is_null() || fd < 0 { return -1; }
    let h = unsafe { cstr_to_string(host) };

    // Resolve address
    let port_str = format!("{}", port);
    let hints = unsafe {
        let mut h: libc::addrinfo = std::mem::zeroed();
        h.ai_family = libc::AF_UNSPEC;
        h.ai_socktype = libc::SOCK_STREAM;
        h
    };
    let mut res: *mut libc::addrinfo = std::ptr::null_mut();
    let c_host = CString::new(h.as_str()).unwrap_or_default();
    let c_port = CString::new(port_str.as_str()).unwrap_or_default();
    let err = unsafe {
        libc::getaddrinfo(c_host.as_ptr(), c_port.as_ptr(), &hints, &mut res)
    };
    if err != 0 || res.is_null() { return -1; }

    unsafe {
        let r = libc::connect(fd as i32, (*res).ai_addr, (*res).ai_addrlen);
        if r == 0 {
            let flag: i32 = 1;
            libc::setsockopt(fd as i32, libc::IPPROTO_TCP, libc::TCP_NODELAY,
                             &flag as *const _ as *const std::ffi::c_void,
                             std::mem::size_of::<i32>() as libc::socklen_t);
        }
        libc::freeaddrinfo(res);
        r as i64
    }
}

#[no_mangle]
pub extern "C" fn mimi_bind(fd: i64, port: i64) -> i64 {
    if fd < 0 { return -1; }
    unsafe {
        let mut addr: libc::sockaddr_in = std::mem::zeroed();
        addr.sin_family = libc::AF_INET as libc::sa_family_t;
        addr.sin_port = (port as u16).to_be();
        addr.sin_addr.s_addr = libc::INADDR_ANY;
        libc::bind(fd as i32, &addr as *const _ as *const libc::sockaddr,
                   std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t) as i64
    }
}

#[no_mangle]
pub extern "C" fn mimi_listen(fd: i64, backlog: i64) -> i64 {
    if fd < 0 { return -1; }
    unsafe { libc::listen(fd as i32, backlog as i32) as i64 }
}

#[no_mangle]
pub extern "C" fn mimi_accept(fd: i64) -> i64 {
    if fd < 0 { return -1; }
    unsafe {
        let mut addr: libc::sockaddr_in = std::mem::zeroed();
        let mut addr_len: libc::socklen_t = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
        let client_fd = libc::accept(
            fd as i32,
            &mut addr as *mut _ as *mut libc::sockaddr,
            &mut addr_len,
        );
        client_fd as i64
    }
}

#[no_mangle]
pub extern "C" fn mimi_send(
    fd: i64,
    data: *const std::ffi::c_char,
    len: i64,
) -> i64 {
    if fd < 0 || data.is_null() { return -1; }
    unsafe {
        libc::send(
            fd as i32,
            data as *const std::ffi::c_void,
            len as usize,
            0,
        ) as i64
    }
}

#[no_mangle]
pub extern "C" fn mimi_recv(
    fd: i64,
    buf_size: i64,
    out_len: *mut i64,
) -> *mut std::ffi::c_char {
    if fd < 0 || buf_size <= 0 { return std::ptr::null_mut(); }
    let size = buf_size as usize;
    let mut buf: Vec<u8> = vec![0u8; size + 1];
    let n = unsafe {
        libc::recv(fd as i32, buf.as_mut_ptr() as *mut std::ffi::c_void, size, 0)
    };
    if n <= 0 {
        if !out_len.is_null() { unsafe { *out_len = 0; } }
        return std::ptr::null_mut();
    }
    buf[n as usize] = 0;
    if !out_len.is_null() { unsafe { *out_len = n as i64; } }
    // Return as CString (caller must free via libc free)
    let result = unsafe { std::ffi::CString::from_vec_unchecked(buf[..=n as usize].to_vec()) };
    result.into_raw()
}

#[no_mangle]
pub extern "C" fn mimi_close(fd: i64) -> i64 {
    if fd < 0 { return -1; }
    unsafe { libc::close(fd as i32) as i64 }
}

// ---------------------------------------------------------------------------
// HTTP client
// ---------------------------------------------------------------------------

fn parse_http_url(url: &str) -> Option<(String, u16, String)> {
    let rest = url.strip_prefix("http://")?;
    if url.starts_with("https://") { return None; }

    let (host_part, path_part) = if let Some(slash_idx) = rest.find('/') {
        let (h, p) = rest.split_at(slash_idx);
        (h, p)
    } else {
        (rest, "/")
    };

    let (host, port) = if let Some(colon_idx) = host_part.find(':') {
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
    stream.write_all(request.as_bytes()).ok()?;

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

    if response.is_empty() { return None; }

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
    if url.is_null() { return std::ptr::null_mut(); }
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
            CString::new(s).unwrap_or_default().into_raw()
        }
        None => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "C" fn mimi_http_post(
    url: *const std::ffi::c_char,
    body: *const std::ffi::c_char,
) -> *mut std::ffi::c_char {
    if url.is_null() { return std::ptr::null_mut(); }
    let u = unsafe { cstr_to_string(url) };
    let b = if body.is_null() { String::new() } else { unsafe { cstr_to_string(body) } };
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
            CString::new(s).unwrap_or_default().into_raw()
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
        return CString::new("[]").unwrap_or_default().into_raw();
    }

    let mut result = String::from("[");
    let elements = unsafe { std::slice::from_raw_parts(data as *const i64, len as usize) };

    for (i, &raw) in elements.iter().enumerate() {
        if i > 0 { result.push(','); }
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
    CString::new(result).unwrap_or_default().into_raw()
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
    if json.is_null() { unsafe { *out_len = 0; } return std::ptr::null_mut(); }
    let s = unsafe { cstr_to_string(json) };
    let bytes = s.as_bytes();
    let mut pos = 0;

    // Skip whitespace
    while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') { pos += 1; }
    if pos >= bytes.len() || bytes[pos] != b'[' {
        unsafe { *out_len = 0; }
        return std::ptr::null_mut();
    }
    pos += 1;

    // Count elements
    let mut count: i64 = 0;
    {
        let mut p = pos;
        loop {
            if p >= bytes.len() { break; }
            while p < bytes.len() && matches!(bytes[p], b' ' | b'\t' | b'\n' | b'\r' | b',') { p += 1; }
            if p >= bytes.len() || bytes[p] == b']' { break; }

            if elem_type == 2 && bytes[p] == b'"' {
                count += 1;
                p += 1;
                loop {
                    if p >= bytes.len() { break; }
                    if bytes[p] == b'\\' { p += 2; continue; }
                    if bytes[p] == b'"' { p += 1; break; }
                    p += 1;
                }
            } else if bytes[p] == b'-' || bytes[p].is_ascii_digit() {
                count += 1;
                if bytes[p] == b'-' { p += 1; }
                while p < bytes.len() && bytes[p].is_ascii_digit() { p += 1; }
                if p < bytes.len() && bytes[p] == b'.' {
                    p += 1;
                    while p < bytes.len() && bytes[p].is_ascii_digit() { p += 1; }
                }
            } else {
                // Skip unknown (true/false/null)
                while p < bytes.len() && !matches!(bytes[p], b']' | b',') { p += 1; }
            }
        }
    }

    // Allocate output array
    let mut data: Vec<i64> = vec![0i64; count as usize];
    pos = 1; // skip initial [
    let mut idx: i64 = 0;
    loop {
        if pos >= bytes.len() { break; }
        while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r' | b',') { pos += 1; }
        if pos >= bytes.len() || bytes[pos] == b']' { break; }
        if idx >= count { break; }

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
                if bytes[pos] == b'"' { pos += 1; }
                let start = pos;
                while pos < bytes.len() && bytes[pos] != b'"' {
                    if bytes[pos] == b'\\' { pos += 2; } else { pos += 1; }
                }
                let slen = pos - start;
                let mut s_bytes = bytes[start..start + slen].to_vec();
                s_bytes.push(0);
                let c_str = unsafe { std::ffi::CString::from_vec_unchecked(s_bytes) };
                data[idx as usize] = c_str.into_raw() as i64;
                if pos < bytes.len() && bytes[pos] == b'"' { pos += 1; }
                idx += 1;
            }
            _ => {
                // Integer
                let neg = if bytes[pos] == b'-' { pos += 1; true } else { false };
                let mut val: i64 = 0;
                while pos < bytes.len() && bytes[pos].is_ascii_digit() {
                    val = val.wrapping_mul(10).wrapping_add((bytes[pos] - b'0') as i64);
                    pos += 1;
                }
                if neg { val = val.wrapping_neg(); }
                data[idx as usize] = val;
                idx += 1;
            }
        }
    }

    let result = data.as_mut_ptr();
    std::mem::forget(data);
    unsafe { *out_len = count; }
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
        return CString::new("[]").unwrap_or_default().into_raw();
    }
    let vals = unsafe { std::slice::from_raw_parts(values, count as usize) };
    let types = if elem_types.is_null() {
        &[] as &[i64]
    } else {
        unsafe { std::slice::from_raw_parts(elem_types, count as usize) }
    };

    let mut result = String::from("[");
    for i in 0..count as usize {
        if i > 0 { result.push(','); }
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
    CString::new(result).unwrap_or_default().into_raw()
}

#[no_mangle]
pub extern "C" fn mimi_tuple_deserialize(
    json: *const std::ffi::c_char,
    count: i64,
    elem_types: *mut i64,
    out_values: *mut i64,
) -> i64 {
    if json.is_null() || out_values.is_null() || count <= 0 { return -1; }
    let s = unsafe { cstr_to_string(json) };
    let bytes = s.as_bytes();
    let mut pos = 0;

    while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') { pos += 1; }
    if pos >= bytes.len() || bytes[pos] != b'[' { return -1; }
    pos += 1;

    let types = if elem_types.is_null() {
        &[] as &[i64]
    } else {
        unsafe { std::slice::from_raw_parts(elem_types, count as usize) }
    };

    let mut idx: i64 = 0;
    loop {
        if pos >= bytes.len() { break; }
        while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r' | b',') { pos += 1; }
        if pos >= bytes.len() || bytes[pos] == b']' { break; }
        if idx >= count { break; }

        let tag = if (idx as usize) < types.len() { types[idx as usize] } else { 0 };
        match tag {
            1 => {
                // Float
                let mut end = pos;
                if end < bytes.len() && bytes[end] == b'-' { end += 1; }
                while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b'.' || bytes[end] == b'e' || bytes[end] == b'E' || bytes[end] == b'+' || bytes[end] == b'-') { end += 1; }
                let num_str = std::str::from_utf8(&bytes[pos..end]).unwrap_or("0");
                let f: f64 = num_str.parse().unwrap_or(0.0);
                unsafe { *out_values.offset(idx as isize) = f64::to_bits(f) as i64; }
                pos = end;
                idx += 1;
            }
            2 => {
                // String
                if bytes[pos] == b'"' { pos += 1; }
                let start = pos;
                while pos < bytes.len() && bytes[pos] != b'"' {
                    if bytes[pos] == b'\\' { pos += 2; } else { pos += 1; }
                }
                let slen = pos - start;
                if slen > 0 {
                    let mut s_bytes = bytes[start..start + slen].to_vec();
                    s_bytes.push(0);
                    let c_str = unsafe { std::ffi::CString::from_vec_unchecked(s_bytes) };
                    unsafe { *out_values.offset(idx as isize) = c_str.into_raw() as i64; }
                } else {
                    unsafe { *out_values.offset(idx as isize) = 0; }
                }
                if pos < bytes.len() && bytes[pos] == b'"' { pos += 1; }
                idx += 1;
            }
            _ => {
                // Integer
                let neg = if bytes[pos] == b'-' { pos += 1; true } else { false };
                let mut val: i64 = 0;
                while pos < bytes.len() && bytes[pos].is_ascii_digit() {
                    val = val.wrapping_mul(10).wrapping_add((bytes[pos] - b'0') as i64);
                    pos += 1;
                }
                if neg { val = val.wrapping_neg(); }
                unsafe { *out_values.offset(idx as isize) = val; }
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
pub extern "C" fn __mimi_extern_test_positive(x: i32) -> i32 { x }

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
pub extern "C" fn __mimi_extern_test_float_identity(x: f64) -> f64 { x }

#[no_mangle]
pub extern "C" fn __mimi_extern_test_struct_by_val(p: __mimi_TestPoint) -> i32 { p.x + p.y }

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
    if s.is_null() { return -1; }
    let str = unsafe { CStr::from_ptr(s) };
    str.to_bytes().len() as i32
}

#[no_mangle]
pub extern "C" fn __mimi_extern_test_nop() {}

#[no_mangle]
pub extern "C" fn __mimi_extern_test_parse_int(json: *const std::ffi::c_char) -> i32 {
    if json.is_null() { return -1; }
    let s = unsafe { cstr_to_string(json) };
    let s = s.trim();
    let neg = s.starts_with('-');
    let digits = s.trim_start_matches('-');
    let val: i32 = digits.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().unwrap_or(0);
    if neg { -val } else { val }
}

#[no_mangle]
pub extern "C" fn __mimi_extern_test_greet(x: i32) -> *mut std::ffi::c_char {
    let msg = format!("Hello {}", x);
    CString::new(msg).unwrap_or_default().into_raw()
}

#[no_mangle]
pub extern "C" fn __mimi_extern_test_json_sum(json: *const std::ffi::c_char) -> i32 {
    if json.is_null() { return -1; }
    let s = unsafe { cstr_to_string(json) };
    let s = s.trim();
    if !s.starts_with('[') { return -1; }
    let inner = s.trim_start_matches('[').trim_end_matches(']');
    let mut sum = 0i32;
    for part in inner.split(',') {
        let part = part.trim();
        if part.is_empty() { continue; }
        if let Ok(n) = part.parse::<i32>() {
            sum = sum.wrapping_add(n);
        }
    }
    sum
}

#[no_mangle]
pub extern "C" fn __mimi_extern_test_segfault() {
    // Deliberate null pointer dereference for testing
    unsafe { std::ptr::write_volatile(std::ptr::null_mut::<i32>(), 42); }
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
pub extern "C" fn test_float_identity(x: f64) -> f64 { __mimi_extern_test_float_identity(x) }

#[no_mangle]
pub extern "C" fn test_strlen(s: *const std::ffi::c_char) -> i32 { __mimi_extern_test_strlen(s) }

#[no_mangle]
pub extern "C" fn test_nop() { __mimi_extern_test_nop() }

#[no_mangle]
pub extern "C" fn test_parse_int(json: *const std::ffi::c_char) -> i32 {
    __mimi_extern_test_parse_int(json)
}

#[no_mangle]
pub extern "C" fn test_json_sum(json: *const std::ffi::c_char) -> i32 {
    __mimi_extern_test_json_sum(json)
}

#[no_mangle]
pub extern "C" fn test_segfault() { __mimi_extern_test_segfault() }

#[no_mangle]
pub extern "C" fn test_abort() { __mimi_extern_test_abort() }

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
pub extern "C" fn test_callback(
    x: i32,
    cb: Option<unsafe extern "C" fn(i32) -> i32>,
) -> i32 {
    __mimi_extern_test_callback(x, cb)
}

// ---------------------------------------------------------------------------
// No_panic signal handlers (POSIX only)
// ---------------------------------------------------------------------------

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod no_panic {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::cell::UnsafeCell;
    use std::cell::Cell;
    #[cfg(standalone)]
    use crate::libc;

    static HANDLERS_INSTALLED: AtomicBool = AtomicBool::new(false);

    const JMP_BUF_SIZE: usize = 128;
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
        libc::SIGSEGV, libc::SIGABRT, libc::SIGBUS,
        libc::SIGILL, libc::SIGFPE,
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
        unsafe {
            libc::signal(libc::SIGSEGV, libc::SIG_DFL);
            libc::signal(libc::SIGABRT, libc::SIG_DFL);
            libc::signal(libc::SIGBUS, libc::SIG_DFL);
            libc::signal(libc::SIGILL, libc::SIG_DFL);
            libc::signal(libc::SIGFPE, libc::SIG_DFL);
        }
        NO_PANIC_JUMP_BUF.with(|buf| {
            let jmp_buf = buf.get();
            if !jmp_buf.is_null() {
                unsafe { siglongjmp(jmp_buf, sig); }
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

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod no_panic {
    #[no_mangle]
    pub extern "C" fn mimi_install_no_panic_handlers() {}

    #[no_mangle]
    pub extern "C" fn mimi_restore_no_panic_handlers() {}
}

#[cfg(not(target_os = "linux"))]
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
static ERROR_HANDLER: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());

#[no_mangle]
pub extern "C" fn mimi_runtime_set_error_handler(
    handler: Option<ErrorHandler>,
) {
    let ptr: *mut std::ffi::c_void = match handler {
        Some(f) => f as *const () as *mut std::ffi::c_void,
        None => std::ptr::null_mut(),
    };
    ERROR_HANDLER.store(ptr, Ordering::Release);
}

#[no_mangle]
pub extern "C" fn mimi_runtime_abort(msg: *const std::ffi::c_char) -> ! {
    if !msg.is_null() {
        let s = unsafe { CStr::from_ptr(msg) };
        eprintln!("[FFI contract violation] {}", s.to_string_lossy());
    } else {
        eprintln!("[FFI contract violation] (no details)");
    }

    let handler_ptr = ERROR_HANDLER.load(Ordering::Acquire);
    if !handler_ptr.is_null() {
        ERROR_HANDLER.store(std::ptr::null_mut(), Ordering::Release); // prevent re-entry
        let handler: ErrorHandler = unsafe { std::mem::transmute::<*mut std::ffi::c_void, ErrorHandler>(handler_ptr) };
        unsafe { handler(msg) };
        std::process::abort();
    }

    eprintln!("Hint: use --skip-verify-ffi to disable contract checking.");
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
    let n = if name.is_null() { String::new() } else { unsafe { cstr_to_string(name) } };
    CAP_TABLE.with(|table| {
        let mut state = table.lock().expect("cap table lock poisoned");
        let id = state.next_id;
        state.next_id += 1;
        state.entries.push(CapEntry { id, name: n, consumed: false });
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
    completed: i32,
    _pad: [u8; 4],
    data: [u8; 64],
}

#[no_mangle]
pub extern "C" fn mimi_future_alloc(_result_size: u64) -> *mut std::ffi::c_void {
    let b = Box::new(MimiFutureRepr { completed: 0, _pad: [0; 4], data: [0; 64] });
    Box::into_raw(b) as *mut std::ffi::c_void
}

#[no_mangle]
pub extern "C" fn mimi_future_free(fut: *mut std::ffi::c_void) {
    if fut.is_null() { return; }
    unsafe { drop(Box::from_raw(fut as *mut MimiFutureRepr)); }
}

#[no_mangle]
pub extern "C" fn mimi_future_set_completed(fut: *mut std::ffi::c_void) {
    if fut.is_null() { return; }
    unsafe { std::ptr::write(fut as *mut i32, 1); }
}

#[no_mangle]
pub extern "C" fn mimi_future_is_completed(fut: *mut std::ffi::c_void) -> i32 {
    if fut.is_null() { return 1; }
    unsafe { std::ptr::read(fut as *const i32) }
}

type PollFn = unsafe extern "C" fn(*mut std::ffi::c_void);

/// Wrapper to make *mut c_void Send (needed for Mutex).
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
    if future.is_null() { return; }
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
            if queue.is_empty() { return; }
            let mut found = None;
            for i in 0..queue.len() {
                let (_, future) = &queue[i];
                let completed = unsafe { std::ptr::read(future.0 as *const i32) };
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
    let n = if name.is_null() { "" } else { unsafe { CStr::from_ptr(name) }.to_str().unwrap_or("") };
    CAP_TABLE.with(|table| {
        let state = table.lock().expect("cap table lock poisoned");
        state.entries.iter().any(|e| e.id == cap && !e.consumed && e.name == n)
    })
}

#[no_mangle]
pub extern "C" fn mimi_cap_consume(cap: i64, name: *const std::ffi::c_char) -> bool {
    let n = if name.is_null() { "" } else { unsafe { CStr::from_ptr(name) }.to_str().unwrap_or("") };
    CAP_TABLE.with(|table| {
        let mut state = table.lock().expect("cap table lock poisoned");
        if let Some(entry) = state.entries.iter_mut().find(|e| e.id == cap && !e.consumed) {
            if entry.name == n {
                entry.consumed = true;
                return true;
            }
        }
        false
    })
}

