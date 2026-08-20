// Mimi language runtime — pure Rust implementation.
//
// This module provides all runtime symbols needed by LLVM-codegened Mimi programs,
// replacing the previous C implementation (mimi_runtime.c). Every function is
// `#[no_mangle] pub extern "C"` so it can be linked from generated machine code.
//
// dead_code is suppressed module-wide (see lib.rs): these symbols are called
// from LLVM IR (invisible to rustc's reachability analysis).

mod epoch;
mod handle;
pub(crate) mod list_string;
pub mod profiler;
pub use epoch::{
    mimi_flow_bump_epoch, mimi_flow_check_epoch, mimi_flow_drop, mimi_flow_epoch,
    mimi_flow_last_error, mimi_flow_pack, mimi_flow_pack_count, mimi_flow_reject_bare_record,
    mimi_flow_unpack, EpochError, TransitionEpoch, EPOCH_ERR_BARE_RECORD, EPOCH_ERR_INVALID,
    EPOCH_ERR_STALE, EPOCH_INITIAL, EPOCH_OK,
};
pub use handle::{
    mimi_handle_last_error, mimi_map_begin_destroy, mimi_map_finish_destroy, mimi_map_generation,
    mimi_map_lease_acquire, mimi_map_lease_count, mimi_map_lease_release, mimi_map_try_size,
    mimi_set_begin_destroy, mimi_set_finish_destroy, mimi_set_generation, mimi_set_lease_acquire,
    mimi_set_lease_count, mimi_set_lease_release, mimi_set_try_size, HandleError, HandleGeneration,
    HANDLE_ERR_DESTROYED, HANDLE_ERR_INVALID, HANDLE_ERR_STALE, HANDLE_OK,
};
pub use list_string::{
    mimi_list_read_string, mimi_list_string_abi_version, mimi_str_box, mimi_str_box_copy,
    mimi_str_split_ll, mimi_str_unbox, LIST_STRING_ABI_CSTR, LIST_STRING_ABI_FAT,
    LIST_STRING_ABI_VERSION, MIMI_ERR_OLD_STRING_ABI, MIMI_STR_MAGIC,
};
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
// ## Handle thread-safety
//
// Map/set handles are raw allocation addresses registered in LIVE_MAPS and
// LIVE_SETS. The registry detects stale/double-destroyed handles but does not
// provide a lease or reference count. Callers must not concurrently destroy a
// map/set while another thread may be using the same handle (P1-03). The
// interpreter/codegen callers already serialize handle access; external FFI
// callers sharing a handle across threads are responsible for the same
// synchronization.
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

// Re-export types used by FFI tests and codegen
// Must match the C layouts exactly.

/// 0.31.23: Element kind for typed list storage.
/// Blind review fix: List elements were previously all stored as *mut c_char (stringified),
/// causing performance overhead and type information loss.
/// Now each list tracks its element type for type-safe operations.
#[repr(i8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListElementKind {
    /// Unknown/unset (legacy compatibility)
    Unknown = 0,
    /// i64 elements stored directly in data array
    I64 = 1,
    /// f64 elements stored directly in data array
    F64 = 2,
    /// bool elements stored as i64 (0/1) in data array
    Bool = 3,
    /// String elements stored as pointers to `MimiStr { magic, ptr, len }`
    /// (0.38.26). The previous C-string (`char*`) layout is `string_abi == 0`
    /// and is rejected by current readers.
    String = 4,
    /// Map handles stored as i64 in data array
    Map = 5,
    /// Set handles stored as i64 in data array
    Set = 6,
    /// Nested list pointers stored as *mut MimiList
    List = 7,
    /// Record pointers stored as *mut c_void
    Record = 8,
}

impl ListElementKind {
    /// Returns the size in bytes of each element for this kind.
    pub fn element_size(self) -> usize {
        match self {
            ListElementKind::Unknown => std::mem::size_of::<*mut std::ffi::c_char>(),
            ListElementKind::I64 => std::mem::size_of::<i64>(),
            ListElementKind::F64 => std::mem::size_of::<f64>(),
            ListElementKind::Bool => std::mem::size_of::<i64>(), // stored as i64
            ListElementKind::String => std::mem::size_of::<*mut std::ffi::c_char>(),
            ListElementKind::Map => std::mem::size_of::<i64>(),
            ListElementKind::Set => std::mem::size_of::<i64>(),
            ListElementKind::List => std::mem::size_of::<*mut MimiList>(),
            ListElementKind::Record => std::mem::size_of::<*mut std::ffi::c_void>(),
        }
    }

    /// Returns true if elements are stored as pointers (need free on list free).
    ///
    /// 0.31.23: Unknown is treated as pointer kind for backward compatibility.
    /// Legacy code that creates lists without setting element_kind will have
    /// their elements freed correctly (assuming they are strings).
    pub fn is_pointer_kind(self) -> bool {
        matches!(
            self,
            ListElementKind::Unknown
                | ListElementKind::String
                | ListElementKind::List
                | ListElementKind::Record
        )
    }
}

/// Runtime-side list representation.
///
/// # Layout / ABI contract (audit 2026-08-05, H-26/H-27)
///
/// `#[repr(C)]` field order is frozen: `{len @0, data @8, owns_data @16,
/// element_kind @17, has_header @18, string_abi @19}`. `has_header` and
/// `string_abi` were APPENDED in the pre-existing alignment padding; total
/// size stays 24 bytes. Native codegen passes its own two-field
/// `{i64 len, i8* data}` lists through `MimiListAbiPrefix` — never through
/// this struct — and the Component ABI treats `*mut MimiList` as an opaque
/// `ListHandle` (component/gen.rs), so the appended fields are invisible to
/// both ABIs.
#[repr(C)]
pub struct MimiList {
    // Fields are pub(crate) for regression tests (src/tests/audit_fix_*);
    // the C ABI sees only the repr(C) layout, which is unchanged.
    pub(crate) len: i64,
    pub(crate) data: *mut *mut std::ffi::c_char,
    // FFI-2: Tracks whether data was allocated by Rust (true) or received from C (false).
    // When true, the data buffer is freed with libc::free (at `data` when
    // has_header=false, at `data - 8` when has_header=true). When false, skip
    // free to avoid wrong allocator.
    pub(crate) owns_data: bool,
    /// 0.31.23: Element kind for typed storage.
    /// Determines how to interpret the data array elements.
    pub(crate) element_kind: ListElementKind,
    /// Audit 2026-08-05 (H-26/H-27, ruling: explicit flag, fail-closed):
    /// true iff `data` was allocated by `alloc_list_data`/`realloc_list_data`
    /// and carries the hidden 8-byte capacity header at `data[-8]`.
    /// Header-less lists (every runtime constructor: str_split, listdir,
    /// walk_dir, args_list, map_keys/values, all from_json builders) NEVER
    /// read or write before `data`; growth paths materialize a header first.
    pub(crate) has_header: bool,
    /// 0.38.26: `List<string>` element ABI. `0` = legacy C-string slots
    /// (rejected by current readers). `2` = fat `{ptr, len}` boxes.
    pub(crate) string_abi: u8,
}

/// Prefix shared with native codegen's by-value `{len, data}` list ABI.
///
/// Codegen-owned lists do not contain the runtime-only ownership and element
/// metadata fields, so ABI helpers must view such pointers through this type
/// instead of creating a reference to the larger `MimiList`.
#[repr(C)]
pub(super) struct MimiListAbiPrefix {
    len: i64,
    data: *mut *mut std::ffi::c_char,
}

impl MimiList {
    /// 0.31.23: Create a new empty MimiList with the specified element kind.
    /// `has_header` starts false (no buffer → no header); growth sets it.
    pub fn new_with_kind(kind: ListElementKind) -> Self {
        MimiList {
            len: 0,
            data: std::ptr::null_mut(),
            owns_data: true,
            element_kind: kind,
            has_header: false,
            string_abi: if kind == ListElementKind::String {
                list_string::LIST_STRING_ABI_FAT
            } else {
                0
            },
        }
    }

    pub fn new_string_list() -> Self {
        Self::new_with_kind(ListElementKind::String)
    }

    /// 0.31.23: Create a MimiList with pre-allocated data and specified element kind.
    ///
    /// Audit 2026-08-05 (H-26): `has_header` defaults to FALSE — every
    /// runtime constructor passes a plain libc::malloc'd array with NO hidden
    /// capacity header, and `mimi_list_free`/`list_cap` rely on the flag to
    /// never touch `data[-8]` for these lists.
    pub fn with_data(
        data: *mut *mut std::ffi::c_char,
        len: i64,
        owns_data: bool,
        kind: ListElementKind,
    ) -> Self {
        MimiList {
            len,
            data,
            owns_data,
            element_kind: kind,
            has_header: false,
            string_abi: if kind == ListElementKind::String {
                list_string::LIST_STRING_ABI_FAT
            } else {
                0
            },
        }
    }

    pub fn with_string_data(data: *mut *mut std::ffi::c_char, len: i64, owns_data: bool) -> Self {
        let mut lst = Self::with_data(data, len, owns_data, ListElementKind::String);
        lst.string_abi = list_string::LIST_STRING_ABI_FAT;
        lst
    }

    /// 0.31.23: Get the element kind of this list.
    pub fn element_kind(&self) -> ListElementKind {
        self.element_kind
    }
}

pub type ValueHandle = i64;
pub type MapHandle = i64;

// P0-10 (batch4/05): these runtime handles cross the LLVM `i64` ABI. Keeping
// them as explicit 64-bit integers (rather than `usize`) prevents silent
// truncation on 32-bit targets.
const _: () = assert!(std::mem::size_of::<ValueHandle>() == std::mem::size_of::<i64>());
const _: () = assert!(std::mem::size_of::<MapHandle>() == std::mem::size_of::<i64>());

// ---------------------------------------------------------------------------
// R-C11: live handle registries (Map / Set; Actor → actor.rs, Quote → quote.rs)
// ---------------------------------------------------------------------------
// 0.38.36: Map/Set handles are packed (index, HandleGeneration) tokens.
// Live-set HashSets are replaced by handle::Table. See handle.rs.

// ---------------------------------------------------------------------------
// Memory allocation helpers
// ---------------------------------------------------------------------------

/// 0.31.22 统一分配器：mimi_alloc/mimi_free 封装
///
/// 盲审修复：禁止直接调用 libc::malloc/free，统一通过 mimi_alloc/mimi_free。
/// - 默认使用 libc::malloc/free（C ABI 兼容）
/// - #[cfg(miri)] 使用 Rust alloc + size header（Miri 可以检测 Rust 分配器的错误）
///
/// SAFETY: 调用者必须确保：
/// - mimi_alloc 返回的指针只能通过 mimi_free 释放
/// - 不能混用 libc::free 和 mimi_free
#[inline]
pub fn mimi_alloc(size: usize) -> *mut std::ffi::c_void {
    #[cfg(miri)]
    {
        // Miri 模式：使用 Rust 分配器 + size header
        // Miri 可以检测 use-after-free、double-free、layout mismatch 等
        use std::alloc::{alloc, Layout};
        // 在分配前添加 size header，以便 mimi_free 可以正确 dealloc
        let header_size = std::mem::size_of::<usize>();
        let total_size = size.saturating_add(header_size).max(1);
        let layout =
            Layout::from_size_align(total_size, 8).unwrap_or_else(|_| std::process::abort());
        // SAFETY: `layout` describes a valid allocation matching the original Layout request
        let base_ptr = unsafe { alloc(layout) };
        if base_ptr.is_null() {
            return std::ptr::null_mut();
        }
        // SAFETY: `base_ptr` was returned by `alloc(layout)` and is non-null; writing to a `usize` at the base is within the allocation bounds
        // 在 header 中存储原始 size
        unsafe {
            *(base_ptr as *mut usize) = size;
        }
        // SAFETY: `mut` points to a valid, properly aligned value
        // 返回跳过 header 的指针
        unsafe { base_ptr.add(header_size) as *mut std::ffi::c_void }
    }
    #[cfg(not(miri))]
    {
        // SAFETY: `size` is a valid, non-negative allocation size
        // 正常模式：使用 libc::malloc（C ABI 兼容）
        unsafe { libc::malloc(size) }
    }
}

/// 0.31.22 统一分配器：mimi_free
///
/// SAFETY: ptr 必须是由 mimi_alloc 返回的指针，且未被释放过。
#[inline]
pub fn mimi_free(ptr: *mut std::ffi::c_void) {
    if ptr.is_null() {
        return;
    }
    #[cfg(miri)]
    {
        // Miri 模式：从 header 读取 size，使用正确的 layout dealloc
        use std::alloc::{dealloc, Layout};
        let header_size = std::mem::size_of::<usize>();
        // SAFETY: `mut` points to a valid, properly aligned value
        // 回退到 header 位置
        let base_ptr = unsafe { (ptr as *mut u8).sub(header_size) };
        // SAFETY: `mut` points to a valid, properly aligned value
        // 读取原始 size
        let size = unsafe { *(base_ptr as *mut usize) };
        let total_size = size.saturating_add(header_size).max(1);
        let layout =
            Layout::from_size_align(total_size, 8).unwrap_or_else(|_| std::process::abort());
        // SAFETY: `layout` describes a valid allocation matching the original Layout request
        unsafe { dealloc(base_ptr, layout) };
    }
    #[cfg(not(miri))]
    {
        // SAFETY: `ptr` was returned by a previous `malloc`/`realloc` call and is not yet freed
        // 正常模式：使用 libc::free
        unsafe { libc::free(ptr) };
    }
}

/// Allocate a C string (null-terminated) using mimi_alloc.
/// The caller is responsible for freeing with mimi_string_free or mimi_free.
fn alloc_c_string(s: &str) -> *mut std::ffi::c_char {
    // SAFETY: `len + 1` is non-zero; the null terminator is written within the allocated buffer.
    let bytes = s.as_bytes();
    let len = bytes.len();
    // 0.31.22 统一分配器：使用 mimi_alloc 替代 libc::malloc
    let ptr = mimi_alloc(len + 1) as *mut u8;
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

/// audit-wave1 (JSON `\uXXXX`): decode one JSON unicode escape starting at the
/// first hex digit (`bytes[pos]`). Returns `Some((char, consumed))` where
/// `consumed` counts the hex-digit bytes after 'u' (4 for BMP chars, 10 for a
/// combined `\uD800-\uDBFF\uDC00-\uDFFF` surrogate pair). serde parity:
/// malformed hex and lone/unpaired surrogates FAIL the parse instead of being
/// silently dropped (old code discarded them → silent data corruption).
fn json_decode_unicode_escape(bytes: &[u8], pos: usize) -> Option<(char, usize)> {
    if pos.checked_add(4)? > bytes.len() {
        return None;
    }
    let hex = std::str::from_utf8(&bytes[pos..pos + 4]).ok()?;
    // 4 hex digits always fit in u16; this fails only on non-hex input.
    let hi = u16::from_str_radix(hex, 16).ok()?;
    if (0xD800..=0xDBFF).contains(&hi) {
        // High surrogate: a low surrogate (\uDC00-\uDFFF) MUST follow.
        if pos.checked_add(10)? > bytes.len() {
            return None;
        }
        if bytes[pos + 4] != b'\\' || bytes[pos + 5] != b'u' {
            return None;
        }
        let hex2 = std::str::from_utf8(&bytes[pos + 6..pos + 10]).ok()?;
        let lo = u16::from_str_radix(hex2, 16).ok()?;
        if !(0xDC00..=0xDFFF).contains(&lo) {
            return None;
        }
        let cp = 0x10000u32 + (((hi as u32) - 0xD800) << 10) + ((lo as u32) - 0xDC00);
        let ch = char::from_u32(cp)?;
        Some((ch, 10))
    } else if (0xDC00..=0xDFFF).contains(&hi) {
        // Lone low surrogate — malformed per serde.
        None
    } else {
        Some((char::from_u32(hi as u32)?, 4))
    }
}

/// JSON-escape a string: wrap in double quotes, escape `"`, `\`, and control chars.
/// JSON unescape: convert escape sequences \", \\, \/, \b, \f, \n, \r, \t, \uXXXX
/// into the actual characters they represent. Used during deserialization.
///
/// Returns `None` on malformed input (bad `\uXXXX` hex, lone/unpaired
/// surrogate, dangling backslash) — callers must fail the JSON parse rather
/// than emit corrupted data (serde parity, audit-wave1).
fn json_unescape(s: &[u8]) -> Option<Vec<u8>> {
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
            // Dangling backslash at EOF — malformed escape.
            return None;
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
                // \uXXXX (or surrogate pair) — malformed input fails the parse.
                let (ch, consumed) = json_decode_unicode_escape(s, i + 1)?;
                let mut buf = [0u8; 4];
                out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                i += consumed;
            }
            c => out.push(c),
        }
        i += 1;
    }
    Some(out)
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
///
/// Audit 2026-08-05 (H-26, fail-closed): reads `data[-8]` ONLY when
/// `has_header` is true (buffer came from `alloc_list_data` /
/// `realloc_list_data`). The old owns_data-based heuristic was contradicted
/// by every runtime constructor: str_split, listdir, walk_dir, args_list,
/// map_keys/values and all from_json builders own libc::malloc'd buffers
/// with NO hidden header, so each `mimi_list_free` performed two
/// out-of-bounds reads (ASan/Valgrind/Miri report) — and a coincidentally
/// negative adjacent byte made `mimi_list_free` free `data - 8` (heap
/// corruption). Header-less lists now never read before `data`.
fn list_cap(list: &MimiList) -> i64 {
    if !list.has_header || list.data.is_null() {
        return 0;
    }
    // SAFETY: `has_header` guarantees `data` came from alloc_list_data /
    // realloc_list_data, so `data - 1` is the allocation base header.
    let hdr = unsafe { *(list.data as *mut i64).offset(-1) };
    // Defense in depth: even with the flag, only trust a marked header.
    if hdr < 0 {
        hdr & 0x7FFF_FFFF_FFFF_FFFF
    } else {
        0
    }
}

/// Grow `lst`'s data buffer to hold at least `new_cap` elements (> len).
///
/// Audit 2026-08-05 (H-27, fail-closed): typed push on a header-less list
/// must NOT `realloc(data - 8)` — `data` itself is the allocation base there
/// (glibc aborts with "realloc(): invalid pointer" on the interior pointer).
/// Instead a fresh headered buffer is materialized and the existing elements
/// are copied over; the old buffer is then freed with origin knowledge:
///   - `has_header`            → free(data - 8)   (alloc_list_data base)
///   - `owns_data && !header`  → free(data)       (plain malloc'd array —
///     every header-less runtime constructor allocates via libc::malloc)
///   - `!owns_data`            → leave it to the C owner
/// After a successful grow the list owns a headered buffer
/// (`has_header = true`, `owns_data = true`). Returns the new data pointer,
/// or null on failure (the list is left unchanged).
fn grow_list_data(lst: &mut MimiList, new_cap: i64) -> *mut *mut std::ffi::c_char {
    let old = lst.data;
    let len = lst.len;
    if lst.has_header {
        // Headered buffer: in-place realloc preserves the header contract.
        let nd = realloc_list_data(old, new_cap);
        if nd.is_null() {
            return std::ptr::null_mut();
        }
        lst.data = nd;
        return nd;
    }
    // Header-less buffer: allocate a fresh headered buffer and copy over.
    let nd = alloc_list_data(new_cap);
    if nd.is_null() {
        return std::ptr::null_mut();
    }
    if !old.is_null() && len > 0 {
        let copy_size =
            match (len as usize).checked_mul(std::mem::size_of::<*mut std::ffi::c_char>()) {
                Some(s) => s,
                None => {
                    // Cannot bound the copy: unwind the fresh allocation
                    // (headered → base is nd - 1) and report failure.
                    // SAFETY: `nd` came from alloc_list_data just above, so
                    // `nd - 1` is its allocation base.
                    unsafe {
                        libc::free((nd as *mut i64).offset(-1) as *mut std::ffi::c_void);
                    }
                    return std::ptr::null_mut();
                }
            };
        // SAFETY: `old` is valid for `len` pointer-sized slots (the list's
        // live elements) and `nd` is a fresh allocation of `new_cap` >= len
        // slots; the regions cannot overlap.
        unsafe {
            std::ptr::copy_nonoverlapping(old as *const u8, nd as *mut u8, copy_size);
        }
    }
    // Origin-aware free of the old buffer (see function docs).
    if !old.is_null() && lst.owns_data {
        // SAFETY: header-less owning data is always a plain libc::malloc
        // array (all runtime constructors), so `old` is the allocation base.
        unsafe {
            libc::free(old as *mut std::ffi::c_void);
        }
    }
    lst.data = nd;
    lst.has_header = true;
    // The list now owns the new buffer even if the old one was C-owned.
    lst.owns_data = true;
    nd
}

/// v0.28.13: Push an i64 element into a MimiList with exponential capacity growth.
/// Uses the hidden header (alloc_list_data/realloc_list_data) once present for
/// O(1) amortized push. Header-less lists (all runtime constructors) get a
/// headered buffer materialized on first growth (audit 2026-08-05, H-27).
/// Modifies list in place (data and len are updated).
/// 0.31.23: Sets element_kind to I64 for typed storage.
///
/// # Safety
/// Pointer arguments must be valid NUL-terminated C strings (unless
/// documented otherwise), live Mimi list pointers from `mimi_list_*` calls,
/// and key/value arrays must have at least `len` valid elements.
#[no_mangle]
pub unsafe extern "C" fn mimi_list_push_i64(list: *mut MimiList, element: i64) {
    if list.is_null() {
        return;
    }
    // SAFETY: `list` points to a valid, properly aligned value
    let lst = unsafe { &mut *list };
    // batch4 P2-3: a corrupt/negative len must not be interpreted as a huge
    // unsigned offset in the growth/write path.
    if lst.len < 0 {
        return;
    }
    // 0.31.23: Mark this list as containing i64 elements.
    lst.element_kind = ListElementKind::I64;
    let len = lst.len;
    let cap = list_cap(lst);
    // MEM-C10 (deep audit): use checked_add to prevent integer overflow on len+1.
    let new_len = match len.checked_add(1) {
        Some(n) => n,
        None => return, // len overflow — can't push more
    };
    if new_len > cap {
        // MEM-C10: use checked_mul for cap*2 to prevent overflow.
        // H-27: when cap == 0 (header-less list) the new capacity must cover
        // the EXISTING elements that get copied over — a flat 4 would
        // overflow a header-less list with len >= 4 during the copy.
        let nc = if cap <= 0 {
            new_len.max(4)
        } else {
            match cap.checked_mul(2) {
                Some(c) => c,
                None => return,
            }
        };
        let nd = grow_list_data(lst, nc);
        if nd.is_null() {
            return;
        }
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

/// 0.31.23: Push an f64 element into a MimiList with exponential capacity growth.
/// Header-less lists get a headered buffer materialized on first growth
/// (audit 2026-08-05, H-27). Sets element_kind to F64 for typed storage.
///
/// # Safety
/// Pointer arguments must be valid NUL-terminated C strings (unless
/// documented otherwise), live Mimi list pointers from `mimi_list_*` calls,
/// and key/value arrays must have at least `len` valid elements.
#[no_mangle]
pub unsafe extern "C" fn mimi_list_push_f64(list: *mut MimiList, element: f64) {
    if list.is_null() {
        return;
    }
    // SAFETY: `list` points to a valid, properly aligned value
    let lst = unsafe { &mut *list };
    // batch4 P2-3: negative list lengths are invalid and would be treated as
    // huge unsigned offsets below.
    if lst.len < 0 {
        return;
    }
    // 0.31.23: Mark this list as containing f64 elements.
    lst.element_kind = ListElementKind::F64;
    let len = lst.len;
    let cap = list_cap(lst);
    let new_len = match len.checked_add(1) {
        Some(n) => n,
        None => return,
    };
    if new_len > cap {
        // H-27: header-less growth must cover the existing elements (see push_i64).
        let nc = if cap <= 0 {
            new_len.max(4)
        } else {
            match cap.checked_mul(2) {
                Some(c) => c,
                None => return,
            }
        };
        let nd = grow_list_data(lst, nc);
        if nd.is_null() {
            return;
        }
        // SAFETY: after growth `nd` has capacity >= `new_len`; writing at index `len` is within bounds
        unsafe {
            *(nd as *mut f64).add(len as usize) = element;
        }
    } else {
        // SAFETY: `len < cap`, so writing at index `len` is within the existing allocation
        unsafe {
            *(lst.data as *mut f64).add(len as usize) = element;
        }
    }
    lst.len = len + 1;
}

/// 0.31.23: Push a string element into a MimiList.
/// The string is copied into a new allocation (caller retains ownership of the input).
/// Header-less lists get a headered buffer materialized on first growth
/// (audit 2026-08-05, H-27). Sets element_kind to String for typed storage.
///
/// # Safety
/// Pointer arguments must be valid NUL-terminated C strings (unless
/// documented otherwise), live Mimi list pointers from `mimi_list_*` calls,
/// and key/value arrays must have at least `len` valid elements.
#[no_mangle]
pub unsafe extern "C" fn mimi_list_push_string(
    list: *mut MimiList,
    element: *const std::ffi::c_char,
) {
    if list.is_null() {
        return;
    }
    // SAFETY: `list` points to a valid, properly aligned value
    let lst = unsafe { &mut *list };
    // batch4 P2-3: reject negative lengths before using them as offsets.
    if lst.len < 0 {
        return;
    }
    // 0.31.23: Mark this list as containing string elements.
    lst.element_kind = ListElementKind::String;
    lst.string_abi = list_string::LIST_STRING_ABI_FAT;
    let len = lst.len;
    let cap = list_cap(lst);
    let new_len = match len.checked_add(1) {
        Some(n) => n,
        None => return,
    };
    // Copy the string into a fat `{ptr, len}` box (0.38.26).
    let element_copy = if element.is_null() {
        list_string::alloc_mimi_str(b"") as *mut std::ffi::c_char
    } else {
        // SAFETY: `element` is a valid null-terminated C string returned by a Mimi allocation function
        let s = unsafe { cstr_to_string(element) };
        list_string::alloc_mimi_str(s.as_bytes()) as *mut std::ffi::c_char
    };
    if new_len > cap {
        // H-27: header-less growth must cover the existing elements (see push_i64).
        let nc = if cap <= 0 {
            new_len.max(4)
        } else {
            match cap.checked_mul(2) {
                Some(c) => c,
                None => {
                    // SAFETY: `element_copy` came from alloc_c_string just above
                    // and is not stored anywhere else (growth failed).
                    mimi_free(element_copy as *mut std::ffi::c_void);
                    return;
                }
            }
        };
        let nd = grow_list_data(lst, nc);
        if nd.is_null() {
            // SAFETY: same as above — the fresh copy never reached a list slot.
            mimi_free(element_copy as *mut std::ffi::c_void);
            return;
        }
        // SAFETY: `nd` has capacity >= `new_len` after growth; writing at index `len` is within bounds
        unsafe {
            *nd.add(len as usize) = element_copy;
        }
    } else {
        // SAFETY: `lst.data` has capacity > `len`; writing at index `len` is within the existing allocation
        unsafe {
            *lst.data.add(len as usize) = element_copy;
        }
    }
    lst.len = len + 1;
}

/// v0.28.13: Grow the data array of a MimiList if needed (exponential growth).
/// Returns the (possibly new) data pointer. The caller is responsible for
/// storing the element at `data[len]` and incrementing `list.len`.
/// This variant works for any element type (not just i64).
///
/// # Safety
/// Pointer arguments must be valid NUL-terminated C strings (unless
/// documented otherwise), live Mimi list pointers from `mimi_list_*` calls,
/// and key/value arrays must have at least `len` valid elements.
#[no_mangle]
pub unsafe extern "C" fn mimi_list_push_grow(
    list: *mut MimiList,
    additional: i64,
) -> *mut *mut std::ffi::c_char {
    if list.is_null() || additional <= 0 {
        return std::ptr::null_mut();
    }
    // SAFETY: `list` was checked non-null; mutable reference is held only within this function.
    let lst = unsafe { &mut *list };
    if lst.len < 0 {
        return std::ptr::null_mut();
    }
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
                    None => {
                        // Unwind the fresh headered buffer (base = new_data - 1).
                        // SAFETY: `new_data` came from alloc_list_data above.
                        unsafe {
                            libc::free((new_data as *mut i64).offset(-1) as *mut std::ffi::c_void);
                        }
                        return std::ptr::null_mut();
                    }
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
        // Audit 2026-08-05 (H-26/H-27): free the old buffer with origin
        // knowledge instead of the old `cap > 0` heuristic. The explicit
        // has_header flag removes the uncertainty the H2 guard worked around:
        //   - headered            → free the allocation base (data - 8)
        //   - owning header-less  → free(data): every header-less runtime
        //     constructor allocates a plain libc::malloc array (the old code
        //     leaked this case because it could not verify the origin)
        //   - C-owned (!owns_data)→ leave it to the C caller
        if !old_data.is_null() {
            if lst.has_header {
                // SAFETY: `has_header` guarantees `old_data - 1` is the
                // allocation base written by alloc_list_data.
                let base = unsafe { (old_data as *mut i64).offset(-1) as *mut std::ffi::c_void };
                unsafe {
                    // SAFETY: `base` is the valid allocation base returned by `alloc_list_data`.
                    libc::free(base);
                }
            } else if lst.owns_data {
                // SAFETY: owning header-less data is a plain libc::malloc
                // array, so `old_data` itself is the allocation base.
                unsafe {
                    libc::free(old_data as *mut std::ffi::c_void);
                }
            }
        }
        lst.data = new_data;
        // The installed buffer carries a header and is runtime-owned, even if
        // the replaced buffer was C-owned.
        lst.has_header = true;
        lst.owns_data = true;
        new_data
    } else {
        old_data
    }
}

// ---------------------------------------------------------------------------
// 0.31.23: Typed list element accessors
// ---------------------------------------------------------------------------
// Blind review fix: List elements were previously accessed without type information,
// leading to potential type confusion. These typed accessors use the element_kind
// field to ensure type-safe access.

/// 0.31.23: Get an i64 element from a list at the given index.
/// Returns 0 if the list is null, index is out of bounds, or element_kind is not I64.
///
/// # Safety
/// Pointer arguments must be valid NUL-terminated C strings (unless
/// documented otherwise), live Mimi list pointers from `mimi_list_*` calls,
/// and key/value arrays must have at least `len` valid elements.
#[no_mangle]
pub unsafe extern "C" fn mimi_list_get_i64(list: *const MimiList, index: i64) -> i64 {
    if list.is_null() {
        return 0;
    }
    // SAFETY: `list` is non-null and points to a valid `MimiList`
    let lst = unsafe { &*list };
    if index < 0 || index >= lst.len {
        return 0;
    }
    if lst.data.is_null() {
        return 0;
    }
    // 0.31.23: Type check - only allow access if element_kind is I64 or Unknown (legacy).
    if !matches!(
        lst.element_kind,
        ListElementKind::I64 | ListElementKind::Unknown
    ) {
        return 0;
    }
    // SAFETY: `lst.data` points to a valid, properly aligned value
    unsafe { *(lst.data as *const i64).add(index as usize) }
}

/// 0.31.23: Get an f64 element from a list at the given index.
/// Returns 0.0 if the list is null, index is out of bounds, or element_kind is not F64.
///
/// # Safety
/// Pointer arguments must be valid NUL-terminated C strings (unless
/// documented otherwise), live Mimi list pointers from `mimi_list_*` calls,
/// and key/value arrays must have at least `len` valid elements.
#[no_mangle]
pub unsafe extern "C" fn mimi_list_get_f64(list: *const MimiList, index: i64) -> f64 {
    if list.is_null() {
        return 0.0;
    }
    // SAFETY: `list` is non-null and points to a valid `MimiList`
    let lst = unsafe { &*list };
    if index < 0 || index >= lst.len {
        return 0.0;
    }
    if lst.data.is_null() {
        return 0.0;
    }
    // 0.31.23: Type check - only allow access if element_kind is F64 or Unknown (legacy).
    if !matches!(
        lst.element_kind,
        ListElementKind::F64 | ListElementKind::Unknown
    ) {
        return 0.0;
    }
    // SAFETY: `lst.data` points to a valid, properly aligned value
    unsafe { *(lst.data as *const f64).add(index as usize) }
}

/// 0.31.23: Get a string element from a list at the given index.
/// Returns null if the list is null, index is out of bounds, or element_kind is not String.
///
/// # Safety
/// Pointer arguments must be valid NUL-terminated C strings (unless
/// documented otherwise), live Mimi list pointers from `mimi_list_*` calls,
/// and key/value arrays must have at least `len` valid elements.
#[no_mangle]
pub unsafe extern "C" fn mimi_list_get_string(
    list: *const MimiList,
    index: i64,
) -> *mut std::ffi::c_char {
    if list.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `list` is non-null and points to a valid `MimiList`
    let lst = unsafe { &*list };
    if index < 0 || index >= lst.len {
        return std::ptr::null_mut();
    }
    if lst.data.is_null() {
        return std::ptr::null_mut();
    }
    // 0.31.23 / 0.38.26: String lists are fat boxes. Legacy C-string ABI
    // is rejected (null), never silently strlen-truncated.
    if !matches!(
        lst.element_kind,
        ListElementKind::String | ListElementKind::Unknown
    ) {
        return std::ptr::null_mut();
    }
    if lst.string_abi != list_string::LIST_STRING_ABI_FAT
        && lst.element_kind == ListElementKind::String
    {
        return std::ptr::null_mut();
    }
    let slot = unsafe { *lst.data.add(index as usize) };
    match unsafe { list_string::read_mimi_str(slot) } {
        Ok((ptr, _)) => ptr,
        Err(_) => std::ptr::null_mut(),
    }
}

/// 0.31.23: Get the element kind of a list.
/// Returns the element kind as an i8 (see ListElementKind enum).
///
/// # Safety
/// Pointer arguments must be valid NUL-terminated C strings (unless
/// documented otherwise), live Mimi list pointers from `mimi_list_*` calls,
/// and key/value arrays must have at least `len` valid elements.
#[no_mangle]
pub unsafe extern "C" fn mimi_list_element_kind(list: *const MimiList) -> i8 {
    if list.is_null() {
        return ListElementKind::Unknown as i8;
    }
    // SAFETY: `list` is non-null and points to a valid `MimiList`
    let lst = unsafe { &*list };
    lst.element_kind as i8
}

/// Crate-visible probe of the explicit header flag (audit 2026-08-05, H-26),
/// for regression tests in src/tests/. Deliberately NOT `#[no_mangle]` —
/// it is not part of the Component/runtime ABI surface.
pub fn mimi_list_has_header_probe(list: *const MimiList) -> bool {
    if list.is_null() {
        return false;
    }
    // SAFETY: `list` is non-null and points to a valid `MimiList`.
    unsafe { (*list).has_header }
}

/// S15/S22: Free a C string allocated by alloc_c_string.
/// Safe to call with null pointer (no-op).
///
/// Audit 2026-08-05 (N-1): free through `mimi_free`, mirroring the
/// `alloc_c_string` → `mimi_alloc` path. Under cfg(miri) `mimi_alloc` uses
/// the Rust allocator + an 8-byte size header; a raw `libc::free` was both
/// the wrong allocator and the wrong base (Miri-detectable UB). In normal
/// builds `mimi_free` IS `libc::free` — behavior is unchanged.
///
/// # Safety
/// Pointer arguments must be valid NUL-terminated C strings (unless
/// documented otherwise), live Mimi list pointers from `mimi_list_*` calls,
/// and key/value arrays must have at least `len` valid elements.
#[no_mangle]
pub unsafe extern "C" fn mimi_string_free(ptr: *mut std::ffi::c_char) {
    if !ptr.is_null() {
        // SAFETY: `ptr` is non-null (checked above) and was allocated by
        // `mimi_alloc` via `alloc_c_string`; mimi_free is the matching
        // deallocator for both miri and normal builds.
        mimi_free(ptr as *mut std::ffi::c_void);
    }
}

/// Free a MimiList and optionally its C string elements.
/// The MimiList struct itself is always heap-allocated via Box in this runtime,
/// so we use Box::from_raw to free it (NOT libc::free, which would be allocator mismatch).
/// FFI-2: Only frees data if `owns_data` is true (Rust-allocated).
/// C-allocated data (owns_data=false) is skipped to avoid wrong-allocator heap corruption.
///
/// v0.28.13 / audit 2026-08-05 (H-26): headered buffers (has_header=true,
/// from the push/grow paths) free the allocation base at data-8; header-less
/// buffers (every runtime constructor) free `data` directly and NEVER read
/// data[-8] — the explicit has_header flag selects the path (the old
/// "negative value at data[-8]" heuristic performed two out-of-bounds reads
/// per header-less free and could free data-8 on heap corruption).
///
/// 0.31.23: Uses element_kind to determine whether elements need freeing.
/// Only String/List/Record elements are heap-allocated pointers; I64/F64/Bool/Map/Set
/// are stored directly in the data array and don't need individual freeing.
///
/// # Safety
/// Pointer arguments must be valid NUL-terminated C strings (unless
/// documented otherwise), live Mimi list pointers from `mimi_list_*` calls,
/// and key/value arrays must have at least `len` valid elements.
#[no_mangle]
pub unsafe extern "C" fn mimi_list_free(list: *mut MimiList, free_elements: bool) {
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
        let element_kind = (*list).element_kind;
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
        // 0.31.23: Only free elements if they are pointer types (String/List/Record).
        // I64/F64/Bool/Map/Set are stored directly and don't need freeing.
        let should_free_elements = free_elements && element_kind.is_pointer_kind();
        // Audit 2026-08-05 (N-1 family): String elements are allocated by
        // alloc_c_string (mimi_alloc) and must be freed through mimi_free —
        // under cfg(miri) mimi_alloc uses the Rust allocator + a size header,
        // so a raw libc::free is both the wrong allocator and the wrong base.
        // Record/List packs stay on libc::free (they are libc::malloc'd under
        // every build).
        if owns_data && !data_ptr.is_null() {
            if should_free_elements {
                for i in 0..safe_count {
                    let e = *data_ptr.add(i);
                    if !e.is_null() {
                        if element_kind == ListElementKind::String {
                            list_string::free_mimi_str(e);
                        } else {
                            libc::free(e as *mut std::ffi::c_void);
                        }
                    }
                }
            }
            let cap = list_cap(&*list);
            if cap > 0 {
                // Headered buffer (has_header): free the allocation base.
                let base = (data_ptr as *mut i64).offset(-1) as *mut std::ffi::c_void;
                libc::free(base);
            } else {
                // Header-less buffer: `data` itself is the malloc'd base.
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
///
/// # Safety
/// Pointer arguments must be valid NUL-terminated C strings (unless
/// documented otherwise), live Mimi list pointers from `mimi_list_*` calls,
/// and key/value arrays must have at least `len` valid elements.
#[no_mangle]
pub unsafe extern "C" fn mimi_list_free_elements(list: *mut MimiList) {
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
                    // N-1 family: String elements come from alloc_c_string
                    // (mimi_alloc) → mimi_free; Record packs are libc::malloc'd
                    // → libc::free (see mimi_list_free for the full rationale).
                    if lst.element_kind == ListElementKind::String {
                        list_string::free_mimi_str(e);
                    } else {
                        libc::free(e as *mut std::ffi::c_void);
                    }
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
    // Audit 2026-08-05 (N-1): allocate via mimi_alloc (same as alloc_c_string)
    // so the pairing deallocator mimi_free / mimi_string_free reverses the
    // exact allocation path under both normal and miri builds. In normal
    // builds mimi_alloc IS libc::malloc — behavior is unchanged.
    let ptr = mimi_alloc(alloc_len) as *mut u8;
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

/// Read one line from standard input and return it as a heap-allocated
/// NUL-terminated C string with trailing whitespace removed (matching the
/// bytecode VM's `input()` trim_end behavior). Returns null on EOF or a
/// read error.
///
/// The caller owns the returned allocation and must free it with
/// `mimi_free` / `mimi_string_free` (both ultimately `libc::free`).
#[no_mangle]
pub extern "C" fn mimi_read_stdin_line() -> *mut std::ffi::c_char {
    let mut input = String::new();
    match std::io::stdin().read_line(&mut input) {
        Ok(0) => std::ptr::null_mut(),
        Ok(_) => alloc_c_string(input.trim_end()),
        Err(_) => std::ptr::null_mut(),
    }
}

// ---------------------------------------------------------------------------
// Integer math
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn __mimi_pow_i64(base: i64, exp: i64) -> i64 {
    // CG-H3: match interpreter — negative exponents and overflow are errors,
    // not silent zero (which collides with legitimate 0**n results).
    if exp < 0 {
        unsafe {
            mimi_runtime_abort(b"negative exponent not supported for integers\0".as_ptr()
                as *const std::ffi::c_char);
        }
    }
    // wave1-review §5.17 (audit 2026-08-05): mirror the VM's exact bound.
    // Bytecode `Op::PowInt` (interp/bytecode/vm.rs) and `builtin_pow`
    // (interp/bytecode/builtins/math.rs) reject exponents above u32::MAX via
    // `u32::try_from`. Without this cap the runtime loop still terminates
    // (O(log exp) squaring) but returns a value for bases in {-1, 0, 1} while
    // the VM traps — an L1 backend divergence (pow(1, 4294967326): VM error,
    // old runtime returned 1).
    if exp > u32::MAX as i64 {
        unsafe {
            mimi_runtime_abort(b"pow: exponent exceeds u32::MAX (integer power)\0".as_ptr()
                as *const std::ffi::c_char);
        }
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
                None => unsafe {
                    mimi_runtime_abort(
                        b"integer overflow in power\0".as_ptr() as *const std::ffi::c_char
                    );
                },
            }
        }
        e >>= 1;
        if e > 0 {
            match b.checked_mul(b) {
                Some(v) => b = v,
                None => unsafe {
                    mimi_runtime_abort(
                        b"integer overflow in power\0".as_ptr() as *const std::ffi::c_char
                    );
                },
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

///
/// # Safety
/// `ptr`/`value` must be a valid `mimi_rc_alloc` allocation (or a
/// runtime-owned C string for `mimi_any_to_string`) and must not
/// be used after the matching release/free.
#[no_mangle]
pub unsafe extern "C" fn mimi_rc_alloc(size: i64) -> *mut std::ffi::c_void {
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
    // SAFETY: `hdr` (== `ptr`) points to uninitialized memory with enough space for `RcHeader`; `strong`/`weak`/`alloc_size` fields are properly aligned for `AtomicI64`
    unsafe {
        // SAFETY: `ptr` points to uninitialized memory with enough space for `RcHeader`; fields are fully initialized.
        (*hdr).strong = AtomicI64::new(1);
        (*hdr).weak = AtomicI64::new(0);
        (*hdr).alloc_size = size;
    }
    // SAFETY: header is initialized before returning pointer to user data at `hdr + 1`.
    unsafe { (hdr.add(1)) as *mut std::ffi::c_void }
}

///
/// # Safety
/// `ptr`/`value` must be a valid `mimi_rc_alloc` allocation (or a
/// runtime-owned C string for `mimi_any_to_string`) and must not
/// be used after the matching release/free.
#[no_mangle]
pub unsafe extern "C" fn mimi_rc_retain(ptr: *mut std::ffi::c_void) {
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

///
/// # Safety
/// `ptr`/`value` must be a valid `mimi_rc_alloc` allocation (or a
/// runtime-owned C string for `mimi_any_to_string`) and must not
/// be used after the matching release/free.
#[no_mangle]
pub unsafe extern "C" fn mimi_rc_release(ptr: *mut std::ffi::c_void) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: `ptr` is non-null (checked above) and was returned by `mimi_rc_alloc`, so the preceding `RcHeader` is valid
    let hdr = unsafe { rc_header_from_ptr(ptr) };
    // SAFETY: atomic decrement with Release ordering; if it returns 1, we own the last strong reference.
    // SAFETY: `hdr` points to a valid, properly aligned value
    // SAFETY: `hdr` was obtained from `rc_header_from_ptr` and points to a valid, properly aligned `RcHeader`
    // B7: TOCTOU analysis — after fetch_sub returns 1 (strong is now 0), a
    // concurrent weak_retain may be running its CAS loop. However, weak_retain
    // checks strong==0 && weak==0 and returns early if both are zero. So:
    // - If weak_retain sees strong=0, weak=0 → it returns (no increment)
    // - If weak_retain sees strong=0, weak>0 → it CAS-increments weak
    // In the latter case, our weak==0 load will see weak>0 and skip dealloc,
    // deferring to the final weak_release. This is the standard Arc drop pattern.
    // SAFETY: `hdr` points to a valid, properly aligned `RcHeader`; the strong count is accessible because `hdr` hasn't been deallocated
    if unsafe { (*hdr).strong.fetch_sub(1, Ordering::Release) } == 1 {
        std::sync::atomic::fence(Ordering::Acquire);
        // H3 fix: Use Acquire ordering for the weak count load to synchronize
        // with the AcqRelease CAS in mimi_rc_weak_retain. Without this, a
        // Relaxed load could see a stale weak count of 0 even after another
        // thread's weak_retain CAS has incremented it, leading to dealloc
        // racing with a concurrent weak retain on a different thread.
        // SAFETY: `hdr` points to a valid `RcHeader`; the weak count is still accessible because no deallocation has occurred (strong count just reached 0, but weak > 0)
        if unsafe { (*hdr).weak.load(Ordering::Acquire) } == 0 {
            // SAFETY: `hdr` points to a valid `RcHeader` with a valid `alloc_size`; strong==0 and weak==0 guarantee exclusive access
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

/// Retain a weak reference.
///
/// Audit 2026-08-05 (N-4) — Arc-class caller contract (EXACT): the CAS guard
/// below only prevents a race against the LAST strong release while the
/// allocation is still live (strong > 0, or weak > 0 keeping the header
/// mapped). It does NOT protect a caller that passes a pointer with no live
/// reference of its own: once both counts reach 0 the header is deallocated,
/// and a dangling call performs RMW on freed memory (UAF / ABA). This is the
/// standard `Arc::downgrade` boundary — callers must hold at least one live
/// strong or weak reference for the duration of this call. There is no
/// cheaper hardening: RC pointers are bare addresses with no handle registry
/// (adding one would put a lock on the hot retain/release path).
///
/// # Safety
/// `ptr`/`value` must be a valid `mimi_rc_alloc` allocation (or a
/// runtime-owned C string for `mimi_any_to_string`) and must not
/// be used after the matching release/free.
#[no_mangle]
pub unsafe extern "C" fn mimi_rc_weak_retain(ptr: *mut std::ffi::c_void) {
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

/// Release a weak reference; deallocates the header when the last reference
/// (strong or weak) is gone.
///
/// Audit 2026-08-05 (N-4): same caller contract as `mimi_rc_weak_retain` —
/// the caller must actually hold the weak reference being released. A call
/// with no live reference RMWs a freed header (UAF); the guard here is the
/// count arithmetic, which is only meaningful for a live allocation.
///
/// # Safety
/// `ptr`/`value` must be a valid `mimi_rc_alloc` allocation (or a
/// runtime-owned C string for `mimi_any_to_string`) and must not
/// be used after the matching release/free.
#[no_mangle]
pub unsafe extern "C" fn mimi_rc_weak_release(ptr: *mut std::ffi::c_void) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: `ptr` is non-null (checked above) and was returned by `mimi_rc_alloc`, so the preceding `RcHeader` is valid
    let hdr = unsafe { rc_header_from_ptr(ptr) };
    // SAFETY: atomic decrement with Release ordering; if it returns 1, we own the last weak reference.
    if unsafe { (*hdr).weak.fetch_sub(1, Ordering::Release) } == 1 {
        std::sync::atomic::fence(Ordering::Acquire);
        // H4 fix: same TOCTOU as H3 — use Acquire ordering for strong count
        // load to synchronize with concurrent strong retain operations.
        if unsafe { (*hdr).strong.load(Ordering::Acquire) } <= 0 {
            // SAFETY: `hdr` points to a valid `RcHeader` with a valid `alloc_size`; weak==0 and strong<=0 under Acquire ordering means no other thread holds a reference
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

/// Upgrade a weak reference to a strong one (returns ptr, or null when the
/// object is already gone).
///
/// Audit 2026-08-05 (N-4): same caller contract as `mimi_rc_weak_retain` —
/// the caller must hold the weak reference being upgraded for the duration
/// of the call. The two-phase CAS below (weak increment first, then strong
/// CAS) is ABA-safe ONLY for a live allocation; a dangling call RMWs freed
/// memory before any count check can reject it. §10-#26 (closed 0.36.109 by
/// design): standard Arc-class boundary; no cheaper hardening exists without
/// a handle registry on the hot path.
///
/// # Safety
/// `ptr`/`value` must be a valid `mimi_rc_alloc` allocation (or a
/// runtime-owned C string for `mimi_any_to_string`) and must not
/// be used after the matching release/free.
#[no_mangle]
pub unsafe extern "C" fn mimi_rc_upgrade(ptr: *mut std::ffi::c_void) -> *mut std::ffi::c_void {
    if ptr.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `ptr` was checked non-null and came from `mimi_rc_alloc`; `rc_header_ref` contract satisfied.
    let hdr = unsafe { rc_header_ref(ptr) };

    // 0.31.22 RC ABA 修复：先增加 weak 计数，防止 header 在 upgrade 期间被释放。
    // 这确保了即使 strong 归零，header 也不会被 dealloc（因为 weak > 0）。
    // 如果 weak 增加失败（strong=0 且 weak=0），说明对象已被释放，返回 null。
    loop {
        let s = hdr.strong.load(Ordering::Acquire);
        let w = hdr.weak.load(Ordering::Relaxed);
        if s == 0 && w == 0 {
            // 对象已被释放或正在释放
            return std::ptr::null_mut();
        }
        // 尝试增加 weak 计数
        match hdr
            .weak
            .compare_exchange_weak(w, w + 1, Ordering::AcqRel, Ordering::Relaxed)
        {
            Ok(_) => break,     // 成功增加 weak，header 现在不会被释放
            Err(_) => continue, // CAS 失败，重试
        }
    }

    // 现在 header 不会被释放（weak > 0），可以安全地尝试升级 strong
    // H19: use Acquire on initial load to match the CAS on the release path,
    // ensuring we see the latest strong count. Relaxed could observe a stale 0
    // and return null even when strong=1, causing a false-negative upgrade failure.
    let mut s = hdr.strong.load(Ordering::Acquire);
    let result = loop {
        if s == 0 {
            // strong 归零，升级失败
            break std::ptr::null_mut();
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
                break ptr;
            }
            Err(new_s) => s = new_s,
        }
    };

    // 释放我们临时增加的 weak 计数
    // 注意：即使升级失败，我们也要释放 weak，否则会导致 weak 泄漏
    if hdr.weak.fetch_sub(1, Ordering::Release) == 1 {
        // 我们是最后一个 weak 引用，且 strong 可能已经归零
        // 检查是否需要释放 header
        std::sync::atomic::fence(Ordering::Acquire);
        if hdr.strong.load(Ordering::Acquire) <= 0 {
            // SAFETY: `hdr` points to a valid, properly aligned value
            let layout = unsafe { rc_dealloc_layout(hdr as *const RcHeader as *mut RcHeader) };
            // SAFETY: 我们观察到 weak==0 且 strong<=0，没有其他线程持有引用
            unsafe {
                std::alloc::dealloc(hdr as *const RcHeader as *mut u8, layout);
            }
        }
    }

    result
}

// ---------------------------------------------------------------------------
// Map (hash table via std::collections::HashMap)
// ---------------------------------------------------------------------------

/// §10-#35 (audit 2026-08-05, closed 2026-08-07): shape of a value buffer
/// the MAP ITSELF allocated (from_json builders). destroy() frees these;
/// caller-supplied values (mimi_map_set / mimi_map_from_list) are never
/// registered and thus never freed by the map — the map cannot know their
/// layout or sharing, and freeing on a guess would trade a leak for UB.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum MapOwnedValueKind {
    /// malloc'd flat i64 pack (product tuple / option / result tagged pack).
    Pack,
    /// malloc'd 16-byte list header `{i64 len, ptr data}` where `data` is a
    /// malloc'd array of `len` owned Pack pointers.
    ListOfPacks,
    /// malloc'd 16-byte `{i64 disc, i64 payload}` result pack: disc == 0
    /// means `Err` and `payload` is an alloc_c_string error message that
    /// must be mimi_free'd; disc == 1 is `Ok` (payload owned by the caller
    /// or an inner object — never freed by the map, see mimi_map_destroy
    /// comment).
    PackErrCString,
    /// Box-allocated `MimiList` built by a `mimi_list_from_json_*` builder
    /// (list itself owns its data + element packs). Freed via
    /// `mimi_list_free(list, /* free_elements */ true)`, which reclaims the
    /// data array and Record element pack bases.
    ListObject,
}

/// Live count of map-owned value buffers (registered − freed). Observable
/// so tests can prove destroy() actually reclaims; near-zero runtime cost.
static MAP_OWNED_VALUE_BALANCE: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// Test/observability hook: number of map-owned value buffers still live.
pub(crate) fn mimi_runtime_map_owned_value_balance() -> i64 {
    MAP_OWNED_VALUE_BALANCE.load(std::sync::atomic::Ordering::Relaxed)
}

/// Test hook: number of value buffers this specific map owns (race-free,
/// per-map — unlike the global balance).
pub(crate) fn mimi_map_owned_value_count(handle: MapHandle) -> i64 {
    handle::with_map(handle, 0, |m| m.owned.len() as i64)
}

/// Free a map-owned value buffer according to its recorded shape.
fn free_map_owned_value(vh: ValueHandle, kind: MapOwnedValueKind) {
    if vh == 0 {
        return;
    }
    match kind {
        MapOwnedValueKind::Pack => {
            // SAFETY: `vh` was recorded as a libc::malloc'd pack base pointer.
            unsafe { libc::free(vh as *mut std::ffi::c_void) };
        }
        MapOwnedValueKind::ListOfPacks => {
            // SAFETY: `vh` was recorded as a malloc'd 16-byte {len, data}
            // header built by mimi_map_from_json_list_product_i64.
            unsafe {
                let base = vh as *const u8;
                let len = *(base as *const i64);
                let data = *(base.add(8) as *const *const i64);
                if len > 0 && len <= 1_000_000 && !data.is_null() {
                    for j in 0..len as isize {
                        let elem = *data.offset(j);
                        if elem != 0 {
                            libc::free(elem as *mut std::ffi::c_void);
                        }
                    }
                    libc::free(data as *mut std::ffi::c_void);
                }
                libc::free(vh as *mut std::ffi::c_void);
            }
        }
        MapOwnedValueKind::PackErrCString => {
            // SAFETY: `vh` was recorded as a malloc'd 16-byte {disc, payload}
            // result pack. disc == 0 → payload is an Err message string we
            // allocated (mimi_free); disc == 1 → payload is an Ok value whose
            // ownership stays with the caller (never freed here).
            unsafe {
                let base = vh as *const i64;
                if *base == 0 {
                    let c = *base.add(1) as *mut std::ffi::c_void;
                    if !c.is_null() {
                        mimi_free(c);
                    }
                }
                libc::free(vh as *mut std::ffi::c_void);
            }
        }
        MapOwnedValueKind::ListObject => {
            // `vh` was recorded as a Box-allocated MimiList built by a
            // mimi_list_from_json_* builder. mimi_list_free reclaims the
            // list struct (Box::from_raw), its owned data array, and Record
            // element pack bases. Map/Set element handles are deliberately
            // not freed (caller may hold them via map_get — bounded leak).
            unsafe {
                mimi_list_free(vh as *mut MimiList, true);
            }
        }
    }
    MAP_OWNED_VALUE_BALANCE.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
}

pub(super) struct MimiMap {
    pub(super) inner: HashMap<String, ValueHandle>,
    /// §10-#35: value buffers this map allocated itself (from_json builders).
    pub(super) owned: HashMap<ValueHandle, MapOwnedValueKind>,
}

/// S4: Return raw pointer instead of &'static mut to avoid aliasing UB.
/// Callers must dereference within a single scope (no two &mut to same handle).
/// S18: abort() instead of panic! — panic across FFI boundary is UB (Rust ABI requirement).
/// R-C11: also aborts on stale (destroyed / never-registered) handles.
/// batch4-05 P1-2: the live-set check and the returned raw pointer are not
/// atomic with respect to `mimi_map_destroy`. Callers MUST NOT share a map
/// handle across threads while one thread can destroy it. The runtime treats
/// cross-thread destroy/use as outside the supported C ABI contract until a
/// per-handle lease/reference-count mechanism lands.
// SAFETY: aborts on invalid/stale handle; caller must ensure exclusive access while live.
fn map_from_handle(handle: MapHandle) -> handle::MapLease {
    match handle::map_acquire(handle) {
        Ok(lease) => lease,
        Err(e) => {
            handle::set_handle_error(e);
            std::process::abort();
        }
    }
}

#[no_mangle]
pub extern "C" fn mimi_map_new() -> MapHandle {
    handle::map_new_handle(MimiMap {
        inner: HashMap::new(),
        owned: HashMap::new(),
    })
}

#[no_mangle]
pub unsafe extern "C" fn mimi_map_destroy(handle: MapHandle) {
    let _ = handle::map_destroy(handle);
}

#[no_mangle]
pub unsafe extern "C" fn mimi_map_size(handle: MapHandle) -> i64 {
    if handle == 0 {
        return 0;
    }
    // SAFETY: handle validated by `map_from_handle`; deref is in a single scope.
    handle::with_map(handle, 0, |m| m.inner.len() as i64)
}

///
/// # Safety
/// `handle` must be a live map handle and `key` must be a valid
/// NUL-terminated C string (or null, which is a no-op).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_has_key(handle: MapHandle, key: *const std::ffi::c_char) -> i32 {
    if handle == 0 || key.is_null() {
        return 0;
    }
    // SAFETY: `handle` is a valid live handle; `map_from_handle`/`set_from_handle` aborts on invalid handles
    let s = unsafe { cstr_to_string(key) };
    // SAFETY: handle validated by `map_from_handle`; deref is in a single scope.
    unsafe { map_from_handle(handle).inner.contains_key(&s) as i32 }
}

///
/// # Safety
/// `handle` must be a live map handle and `key` must be a valid
/// NUL-terminated C string (or null, which is a no-op).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_get(
    handle: MapHandle,
    key: *const std::ffi::c_char,
) -> ValueHandle {
    if handle == 0 || key.is_null() {
        return 0;
    }
    // SAFETY: `key` is a valid null-terminated C string returned by a Mimi allocation function
    let s = unsafe { cstr_to_string(key) };
    // SAFETY: handle validated by `map_from_handle`; deref is in a single scope.
    unsafe { map_from_handle(handle).inner.get(&s).copied().unwrap_or(0) }
}

#[no_mangle]
pub unsafe extern "C" fn mimi_map_clone(handle: MapHandle) -> MapHandle {
    if handle == 0 {
        return 0;
    }
    // SAFETY: handle validated by map_from_handle; deref is in a single scope.
    let src = map_from_handle(handle);
    handle::map_new_handle(MimiMap {
        inner: src.inner.clone(),
        // The clone does not own any value buffers; those remain owned by
        // the source map (or by the value producer). Keeping this empty
        // prevents double frees when both maps are destroyed.
        owned: HashMap::new(),
    })
}

/// Insert `value` under `key` in an existing map.
///
/// # Safety
/// `handle` must be a live map handle. `key` must be a valid
/// NUL-terminated C string (or null, which is a no-op).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_set(
    handle: MapHandle,
    key: *const std::ffi::c_char,
    value: ValueHandle,
) {
    if handle == 0 || key.is_null() {
        return;
    }
    // SAFETY: `key` is a valid null-terminated C string returned by a Mimi allocation function
    let s = unsafe { cstr_to_string(key) };
    // SAFETY: handle validated by `map_from_handle`; deref is in a single scope.
    unsafe {
        map_from_handle(handle).inner.insert(s, value);
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
///
/// # Safety
/// `ptr`/`value` must be a valid `mimi_rc_alloc` allocation (or a
/// runtime-owned C string for `mimi_any_to_string`) and must not
/// be used after the matching release/free.
#[no_mangle]
pub unsafe extern "C" fn mimi_any_to_string(value: ValueHandle) -> *mut std::ffi::c_char {
    const MIN_HEAP: usize = 1_048_576; // 1MB — below this is definitely not a heap ptr
    const MAX_ADDR: usize = usize::MAX - 4096;
    // C12: bounded scan. 1 MiB covers long Mimi strings (up to 64 MiB is
    // possible) while still bounding per-call work for untyped Any values.
    const MAX_BOUNDED_SCAN: usize = 1_048_576;

    // Bit-0 = 0: could be an aligned heap pointer (string), or an even integer.
    // Validate before treating as pointer.
    let value_addr = value as usize;
    if value & 1 == 0 && (MIN_HEAP..MAX_ADDR).contains(&value_addr) && value % 8 == 0 {
        let ptr = value as *const u8;
        // SAFETY: `libc::sysconf`/`libc::mincore` are async-signal-safe POSIX functions
        // C12 (deep audit): a large *untagged* integer (e.g. `0x7FFF_FFFF_F000`)
        // satisfies the heuristic above but points at unmapped memory, so the
        // first read below would SIGSEGV. Probe whether the address is actually
        // mapped (mincore) and only scan within that mapped page, so we never
        // dereference memory we don't own.
        // SAFETY: `libc::sysconf` is async-signal-safe and has no preconditions beyond a valid POSIX constant
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let page_size = if page_size == 0 { 4096 } else { page_size };
        let mut len: usize = 0;
        while len < MAX_BOUNDED_SCAN {
            let cur = value_addr + len;
            let page_start = (cur / page_size) * page_size;
            let page_offset = cur - page_start;
            let chunk = page_size
                .saturating_sub(page_offset)
                .min(MAX_BOUNDED_SCAN - len);
            if chunk == 0 {
                break;
            }
            let mut mvec: u8 = 0;
            // SAFETY: `libc::mincore` is async-signal-safe; `page_start` is page-aligned.
            let mapped =
                unsafe { libc::mincore(page_start as *mut std::ffi::c_void, page_size, &mut mvec) };
            if mapped != 0 {
                break;
            }
            // SAFETY: mincore confirmed this page is mapped; scan/copy is
            // bounded to the page and to MAX_BOUNDED_SCAN, and stops at NUL.
            unsafe {
                for i in 0..chunk {
                    let byte = *ptr.add(len + i);
                    if byte == 0 {
                        let found_len = len + i;
                        // Found a NUL terminator — likely a real C string.
                        // N-1: mimi_alloc pairs with mimi_string_free (see fn docs).
                        let buf = mimi_alloc(found_len + 1) as *mut u8;
                        if buf.is_null() {
                            return std::ptr::null_mut();
                        }
                        if found_len > 0 {
                            std::ptr::copy_nonoverlapping(ptr, buf, found_len);
                        }
                        *buf.add(found_len) = 0;
                        return buf as *mut std::ffi::c_char;
                    }
                }
            }
            len += chunk;
        }
        // C12: no null within the bounded scan — treat as large integer (≥1MB) and
        // format as hex to avoid reading arbitrary memory for 1MB.
        // N-1: mimi_alloc pairs with mimi_string_free (see fn docs); the
        // buffer is 24 bytes and the format string "0x%lx\0" writes at most
        // ~20 bytes on 64-bit. Null check below guards against OOM.
        let buf = mimi_alloc(24) as *mut std::ffi::c_char;
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
    // N-1: mimi_alloc pairs with mimi_string_free (see fn docs).
    let buf = mimi_alloc(24) as *mut std::ffi::c_char;
    if buf.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `buf` is non-null (checked above) and 24 bytes; `snprintf` is bounded to `size` and will not overflow
    unsafe {
        libc::snprintf(buf, 24, b"%ld\0".as_ptr() as *const _, value as i64);
    }
    buf
}

#[no_mangle]
/// Interpret a ValueHandle as an integer. If the handle looks like a C string
/// (per `safe_c_string_from_handle` — aligned, mapped, NUL-terminated), parse
/// it as a decimal integer; otherwise the handle IS the integer value.
/// Used by codegen `to_int` on `Any` (e.g. `map_get` values), which arrive as
/// untyped i64 handles and cannot be distinguished statically.
pub extern "C" fn mimi_any_to_int(value: ValueHandle) -> i64 {
    if let Some(s) = safe_c_string_from_handle(value) {
        parse_c_decimal_i64(&s)
    } else {
        value as i64
    }
}

/// strtol-style decimal parse (whitespace + optional sign + leading digits;
/// no digits parses as 0; trailing garbage ignored). Mirrors the semantics of
/// the libc `strtol` used by codegen's string-to-int path so both backends
/// agree. Returns 0 for empty/whitespace-only input.
fn parse_c_decimal_i64(s: &str) -> i64 {
    let bytes = s.trim_start().as_bytes();
    let mut i = 0usize;
    let mut neg = false;
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        neg = bytes[i] == b'-';
        i += 1;
    }
    let mut acc: u64 = 0;
    let mut any = false;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        any = true;
        acc = acc
            .saturating_mul(10)
            .saturating_add((bytes[i] - b'0') as u64);
        i += 1;
    }
    if !any {
        return 0;
    }
    if neg {
        // strtol saturates at i64::MIN; |i64::MIN| = 2^63 is representable as u64.
        if acc > (i64::MAX as u64) + 1 {
            return i64::MIN;
        }
        if acc == (i64::MAX as u64) + 1 {
            return i64::MIN;
        }
        -(acc as i64)
    } else if acc > i64::MAX as u64 {
        i64::MAX
    } else {
        acc as i64
    }
}

#[no_mangle]
/// Interpret a ValueHandle as a float. If the handle looks like a C string,
/// parse it as a float; otherwise convert the integer handle to float.
/// Mirrors `mimi_any_to_int` for the `to_float` builtin.
pub extern "C" fn mimi_any_to_float(value: ValueHandle) -> f64 {
    if let Some(s) = safe_c_string_from_handle(value) {
        s.trim_start().parse::<f64>().unwrap_or(0.0)
    } else {
        value as f64
    }
}

///
/// # Safety
/// `handle` must be a live map handle and `key` must be a valid
/// NUL-terminated C string (or null, which is a no-op).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_remove(handle: MapHandle, key: *const std::ffi::c_char) -> i32 {
    if handle == 0 || key.is_null() {
        return 0;
    }
    // SAFETY: `handle` is a valid live handle; `map_from_handle`/`set_from_handle` aborts on invalid handles
    let s = unsafe { cstr_to_string(key) };
    // SAFETY: handle validated by `map_from_handle`; deref is in a single scope.
    unsafe { map_from_handle(handle).inner.remove(&s).is_some() as i32 }
}

/// RT-H4 helper: probe whether `[ptr, ptr+len)` spans only mapped pages.
/// Returns 1 on mapped, 0 otherwise. Never dereferences; mincore only.
/// Used by legacy display codegen to distinguish an `Err(string)` slot that
/// stores a bare NUL-terminated data pointer (mincore(field0) fails, field0
/// is payload bytes) from one that stores a `{ptr,len}` struct pointer
/// (mincore(field0) succeeds, field0 is the data pointer).
///
/// # Safety
/// `ptr`/`value` must be a valid `mimi_rc_alloc` allocation (or a
/// runtime-owned C string for `mimi_any_to_string`) and must not
/// be used after the matching release/free.
#[no_mangle]
pub unsafe extern "C" fn mimi_runtime_ptr_readable(ptr: *const u8, len: i64) -> i64 {
    if ptr.is_null() || len <= 0 {
        return 0;
    }
    // batch4 P2-5: a huge untrusted len used to make the page loop scan the
    // whole address space. Bound the probe to a sane span; callers that need
    // a larger span should use a checked helper instead.
    const MAX_READABLE_SPAN: i64 = 1 << 20;
    if len > MAX_READABLE_SPAN {
        return 0;
    }
    // SAFETY: `libc::sysconf`/`libc::mincore` are async-signal-safe POSIX
    // functions; mincore only queries page mappings, never dereferences.
    unsafe {
        let page_size = libc::sysconf(libc::_SC_PAGESIZE) as usize;
        let page_size = if page_size == 0 { 4096 } else { page_size };
        let start = ptr as usize;
        let end = start.saturating_add(len as usize);
        let first = start & !(page_size - 1);
        let last = if end == 0 {
            0
        } else {
            (end - 1) & !(page_size - 1)
        };
        let mut page = first;
        loop {
            let mut mvec: u8 = 0;
            let r = libc::mincore(page as *mut std::ffi::c_void, page_size, &mut mvec);
            if r != 0 {
                return 0;
            }
            if page == last {
                break;
            }
            page += page_size;
        }
        1
    }
}

#[no_mangle]
/// RT-H4 helper: treat an aligned `ValueHandle` as a C string only if mincore
/// says the page is mapped and a NUL terminator appears within a bounded scan.
/// Alignment remains part of the untyped `Any` pointer-vs-integer heuristic.
fn safe_c_string_from_handle(handle: ValueHandle) -> Option<String> {
    safe_c_string_from_handle_impl(handle, true)
}

/// Decode a pointer that is already known by its ABI position to be a C
/// string. C string literals and byte buffers have byte alignment, so this
/// path intentionally skips the aligned-handle heuristic used for `Any`.
fn safe_c_string_from_ptr(ptr: *const std::ffi::c_char) -> Option<String> {
    safe_c_string_from_handle_impl(ptr as ValueHandle, false)
}

fn safe_c_string_from_handle_impl(handle: ValueHandle, require_alignment: bool) -> Option<String> {
    const MIN_HEAP: usize = 1_048_576;
    // 1 MiB bounded scan for untyped map/any string values; see P1-1.
    const MAX_BOUNDED_SCAN: usize = 1_048_576;
    let addr = handle as usize;
    if addr < MIN_HEAP || (require_alignment && handle % 8 != 0) {
        return None;
    }
    // SAFETY: `libc::sysconf`/`libc::mincore` are async-signal-safe POSIX functions
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
    let page_size = if page_size == 0 { 4096 } else { page_size };
    let end = addr.checked_add(MAX_BOUNDED_SCAN)?;
    let ptr = handle as *const u8;
    // RT-H2 soft harden: copy bytes into a local buffer while scanning so a
    // concurrent munmap after mincore cannot corrupt the String we build from
    // a live slice. Residual race remains on the individual byte loads
    // themselves (cannot close fully without process_vm_readv / userfaultfd).
    let mut local: Vec<u8> = Vec::with_capacity(MAX_BOUNDED_SCAN);
    let mut offset = 0usize;
    while offset < MAX_BOUNDED_SCAN {
        let cur = addr + offset;
        let page_start = (cur / page_size) * page_size;
        let page_end = page_start.saturating_add(page_size).min(end);
        let chunk_limit = page_end.saturating_sub(cur);
        if chunk_limit == 0 {
            break;
        }
        let mut mvec: u8 = 0;
        // SAFETY: `libc::mincore` is async-signal-safe; `page_start` is page-aligned.
        let mapped =
            unsafe { libc::mincore(page_start as *mut std::ffi::c_void, page_size, &mut mvec) };
        if mapped != 0 {
            return None;
        }
        let mut scanned = 0usize;
        // SAFETY: mincore confirmed this page is mapped; scan/copy is bounded
        // to this page, MAX_BOUNDED_SCAN, and stops at the first NUL.
        unsafe {
            while scanned < chunk_limit {
                let b = *ptr.add(offset + scanned);
                if b == 0 {
                    // Re-check mapping before trusting the snapshot.
                    let mut mvec2: u8 = 0;
                    if libc::mincore(page_start as *mut std::ffi::c_void, page_size, &mut mvec2)
                        != 0
                    {
                        return None;
                    }
                    return Some(String::from_utf8_lossy(&local).into_owned());
                }
                local.push(b);
                scanned += 1;
            }
        }
        offset += scanned;
    }
    None
}

/// §10-#31 (audit 2026-08-05, closed 2026-08-07): true when every page
/// covering `[addr, addr + len)` passes mincore (i.e. is resident-mapped).
/// `addr` need not be page-aligned; each covering page is probed from its
/// aligned start. Zero-length spans are vacuously mapped.
fn pages_mapped(addr: usize, len: usize) -> bool {
    if len == 0 {
        return true;
    }
    let end = match addr.checked_add(len) {
        Some(e) => e,
        None => return false,
    };
    // SAFETY: `libc::sysconf` is async-signal-safe with a valid POSIX constant
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
    let page_size = if page_size == 0 { 4096 } else { page_size };
    let mut page = (addr / page_size) * page_size;
    while page < end {
        let mut mvec: u8 = 0;
        // SAFETY: `libc::mincore` is async-signal-safe; `page` is page-aligned
        let mapped = unsafe { libc::mincore(page as *mut std::ffi::c_void, page_size, &mut mvec) };
        if mapped != 0 {
            return false;
        }
        page += page_size;
    }
    true
}

static PRODUCT_HANDLE_WARNED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// §10-#31: read a heap-packed product tuple of `n` i64 fields from a
/// ValueHandle ONLY after alignment + mincore probing. The product
/// serializers previously called `from_raw_parts` on any non-null handle,
/// segfaulting on corrupt/foreign handles. Returns None when the handle is
/// not a plausible mapped heap pointer (warns once per process, fail-loud).
fn safe_read_product_fields(handle: ValueHandle, n: usize) -> Option<Vec<i64>> {
    const MIN_HEAP: usize = 1_048_576;
    let addr = handle as usize;
    if n == 0 || addr < MIN_HEAP || handle % 8 != 0 {
        if !PRODUCT_HANDLE_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            eprintln!(
                "[mimi runtime] product value handle {:#x} is not a plausible heap pointer — serialized as zeros",
                handle
            );
        }
        return None;
    }
    let byte_len = n * std::mem::size_of::<i64>();
    if !pages_mapped(addr, byte_len) {
        if !PRODUCT_HANDLE_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            eprintln!(
                "[mimi runtime] product value handle {:#x} points at unmapped memory — serialized as zeros",
                handle
            );
        }
        return None;
    }
    // SAFETY: mincore confirmed every page covering [handle, handle+byte_len)
    // is mapped and `handle` is 8-aligned, so `n` i64 reads are in bounds.
    Some(unsafe { std::slice::from_raw_parts(handle as *const i64, n).to_vec() })
}

///
/// # Safety
/// Pointer arguments must be valid NUL-terminated C strings (unless
/// documented otherwise), live Mimi list pointers from `mimi_list_*` calls,
/// and key/value arrays must have at least `len` valid elements.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_list(
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
    // M7 (0.35.37): the 1M cap silently DROPPED entries beyond the limit
    // (red line #2: no silent error swallowing). Cap loudly: warn once so a
    // caller that hands a huge n (e.g. from a corrupted length) is told the
    // tail was discarded instead of wondering where its keys went.
    let raw_n = n;
    let n = n.min(1_000_000);
    if n < raw_n {
        eprintln!(
            "[mimi runtime] mimi_map_from_list: n = {} exceeds the 1M safety cap; \
             only the first 1M entries are inserted, {} were dropped (FFI caller contract: \
             arrays must have >= n elements)",
            raw_n,
            raw_n - n
        );
    }
    let mut map_lease = match handle::map_acquire(handle) {
        Ok(l) => l,
        Err(_) => return handle,
    };
    let mut warned_bad_key = false;
    for i in 0..n {
        // C6: We only have the caller's word that arrays have >= n elements.
        // We mitigate by capping n at 1M, but the real fix requires a
        // different API that takes slices. For now, validate each array slot
        // is mapped before reading the handle, and validate each key handle
        // looks like a plausible heap pointer before dereference.
        let idx = i as usize;
        let key_slot_addr = keys as usize + idx * std::mem::size_of::<ValueHandle>();
        let val_slot_addr = values as usize + idx * std::mem::size_of::<ValueHandle>();
        if !pages_mapped(key_slot_addr, std::mem::size_of::<ValueHandle>())
            || !pages_mapped(val_slot_addr, std::mem::size_of::<ValueHandle>())
        {
            eprintln!(
                "[mimi runtime] mimi_map_from_list: array slot {} is not mapped;                  refusing to read beyond the caller-provided arrays",
                i
            );
            break;
        }
        // SAFETY: pages_mapped confirmed the slot is resident; keys/values are
        // non-null and n is capped at 1M.
        let key_handle = unsafe { *keys.add(idx) };
        let val_handle = unsafe { *values.add(idx) };
        // RT-H1/H4: map keys are ABI-declared C strings, so their pointers may
        // be byte-aligned (unlike untyped Any handles). Still require the same
        // mapped-page and bounded-NUL validation before dereferencing.
        // M7: a failed key check is diagnosed once (not silent) — it means
        // the caller's array contains a wild/foreign handle, and the pair is
        // skipped rather than inserted under a garbage key.
        if let Some(s) = safe_c_string_from_ptr(key_handle as *const std::ffi::c_char) {
            // SAFETY: map_ptr is the just-allocated map (handle != 0).
            map_lease.inner.insert(s, val_handle);
        } else if !warned_bad_key {
            warned_bad_key = true;
            eprintln!(
                "[mimi runtime] mimi_map_from_list: key handle {:#x} at index {} is not a \
                 plausible mapped C string — entry skipped (and any further bad keys are \
                 silently skipped)",
                key_handle, i
            );
        }
    }
    handle
}

fn mimi_map_collect(handle: MapHandle, collect_values: bool) -> *mut MimiList {
    if handle == 0 {
        let list = Box::new(MimiList::new_with_kind(ListElementKind::String));
        return Box::into_raw(list);
    }
    // SAFETY: handle validated by `map_from_handle`; shared reference is in a single scope.
    let map = map_from_handle(handle);
    let len = map.inner.len() as i64;
    if len == 0 {
        let list = Box::new(MimiList::new_with_kind(ListElementKind::String));
        return Box::into_raw(list);
    }

    // Use libc::malloc for the data pointer to ensure it is compatible with
    // libc::free (which mimi_list_free uses). Rust Vec uses jemalloc/allocator
    // which may not be compatible with libc::free on all platforms (e.g. MSVC).
    // H18: use checked_mul to prevent integer overflow on large maps.
    let data_size = match (len as usize).checked_mul(std::mem::size_of::<*mut std::ffi::c_char>()) {
        Some(s) => s,
        None => return Box::into_raw(Box::new(MimiList::new_with_kind(ListElementKind::String))),
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
                list_string::alloc_mimi_str(k.as_bytes()) as *mut std::ffi::c_char
            };
            // SAFETY: data_ptr is valid, i is within bounds.
            unsafe {
                *data_ptr.add(i) = entry;
            }
        }
    }
    // 0.31.23: keys are strings, values are ValueHandles (treated as unknown)
    let kind = if collect_values {
        ListElementKind::Unknown
    } else {
        ListElementKind::String
    };
    let list = Box::new(MimiList::with_data(data_ptr, len, !collect_values, kind));
    Box::into_raw(list)
}

#[no_mangle]
pub unsafe extern "C" fn mimi_map_keys(handle: MapHandle) -> *mut MimiList {
    mimi_map_collect(handle, false)
}

#[no_mangle]
pub unsafe extern "C" fn mimi_map_values(handle: MapHandle) -> *mut MimiList {
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
///
/// # Safety
/// Pointer arguments must be valid NUL-terminated C strings (unless
/// documented otherwise), live Mimi list pointers from `mimi_list_*` calls,
/// and key/value arrays must have at least `len` valid elements.
#[no_mangle]
pub unsafe extern "C" fn mimi_str_clone(ptr: *const std::ffi::c_char, len: i64) -> ValueHandle {
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
    // N-1: mimi_alloc keeps every runtime-owned C string on the single
    // mimi_alloc/mimi_free pairing (miri-correct; libc::malloc in normal
    // builds — behavior unchanged).
    let buf = mimi_alloc(alloc_len) as *mut u8;
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
///
/// # Safety
/// Pointer arguments must be valid NUL-terminated C strings (unless
/// documented otherwise), live Mimi list pointers from `mimi_list_*` calls,
/// and key/value arrays must have at least `len` valid elements.
#[no_mangle]
pub unsafe extern "C" fn mimi_json_escape_string(
    ptr: *const std::ffi::c_char,
) -> *mut std::ffi::c_char {
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

/// Byte offset of the first occurrence of `needle` in `haystack`, or -1.
/// Unlike C `strstr`, this uses explicit lengths so embedded NUL bytes are
/// searched correctly (P1-13).
///
/// # Safety
/// Both pointers must be valid for their corresponding lengths; negative
/// lengths are rejected by this function but the pointers themselves must
/// originate from live Mimi string values.
#[no_mangle]
pub unsafe extern "C" fn mimi_str_index_of(
    haystack: *const std::ffi::c_char,
    hay_len: i64,
    needle: *const std::ffi::c_char,
    needle_len: i64,
) -> i64 {
    if haystack.is_null() || needle.is_null() || hay_len < 0 || needle_len < 0 {
        return -1;
    }
    if needle_len == 0 {
        return 0;
    }
    if hay_len < needle_len {
        return -1;
    }
    // SAFETY: codegen passes pointers with matching explicit lengths from
    // Mimi string values; negative lengths are rejected above. Slices live
    // only for the duration of the search.
    let hay = unsafe { std::slice::from_raw_parts(haystack as *const u8, hay_len as usize) };
    let needle = unsafe { std::slice::from_raw_parts(needle as *const u8, needle_len as usize) };
    let n = needle_len as usize;
    hay.windows(n)
        .position(|w| w == needle)
        .map(|i| i as i64)
        .unwrap_or(-1)
}

/// Returns 1 when `prefix` is a prefix of `haystack`, using explicit byte
/// lengths so embedded NUL bytes are not treated as terminators.
///
/// # Safety
/// Both pointers must be valid for their corresponding lengths; prefix_len
/// is checked against hay_len before slicing.
#[no_mangle]
pub unsafe extern "C" fn mimi_str_starts_with(
    haystack: *const std::ffi::c_char,
    hay_len: i64,
    prefix: *const std::ffi::c_char,
    prefix_len: i64,
) -> i64 {
    if haystack.is_null() || prefix.is_null() || hay_len < 0 || prefix_len < 0 {
        return 0;
    }
    if prefix_len > hay_len {
        return 0;
    }
    if prefix_len == 0 {
        return 1;
    }
    // SAFETY: lengths are validated non-negative and prefix_len <= hay_len;
    // pointers come from Mimi string values passed by codegen.
    let hay = unsafe { std::slice::from_raw_parts(haystack as *const u8, hay_len as usize) };
    let prefix = unsafe { std::slice::from_raw_parts(prefix as *const u8, prefix_len as usize) };
    i64::from(hay[..prefix_len as usize] == *prefix)
}

/// Returns 1 when `suffix` is a suffix of `haystack`, using explicit byte
/// lengths so embedded NUL bytes are not treated as terminators.
///
/// # Safety
/// Both pointers must be valid for their corresponding lengths; suffix_len
/// is checked against hay_len before slicing.
#[no_mangle]
pub unsafe extern "C" fn mimi_str_ends_with(
    haystack: *const std::ffi::c_char,
    hay_len: i64,
    suffix: *const std::ffi::c_char,
    suffix_len: i64,
) -> i64 {
    if haystack.is_null() || suffix.is_null() || hay_len < 0 || suffix_len < 0 {
        return 0;
    }
    if suffix_len > hay_len {
        return 0;
    }
    if suffix_len == 0 {
        return 1;
    }
    // SAFETY: lengths are validated non-negative and suffix_len <= hay_len;
    // pointers come from Mimi string values passed by codegen.
    let hay = unsafe { std::slice::from_raw_parts(haystack as *const u8, hay_len as usize) };
    let suffix = unsafe { std::slice::from_raw_parts(suffix as *const u8, suffix_len as usize) };
    i64::from(hay[hay_len as usize - suffix_len as usize..] == *suffix)
}

///
/// # Safety
/// Pointer arguments must be valid NUL-terminated C strings (unless
/// documented otherwise), live Mimi list pointers from `mimi_list_*` calls,
/// and key/value arrays must have at least `len` valid elements.
#[no_mangle]
pub unsafe extern "C" fn mimi_str_concat(
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

/// Length-aware string concatenation. Unlike `mimi_str_concat`, this uses the
/// explicit byte lengths so embedded NUL bytes are preserved (batch4/02
/// P1-1). The returned buffer is NUL-terminated for C-string consumers, but
/// codegen wraps it with the computed total length.
///
/// # Safety
/// Pointers must be valid for the corresponding byte lengths; negative
/// lengths are rejected.
#[no_mangle]
pub unsafe extern "C" fn mimi_str_concat_ll(
    a: *const std::ffi::c_char,
    a_len: i64,
    b: *const std::ffi::c_char,
    b_len: i64,
) -> *mut std::ffi::c_char {
    if a_len < 0 || b_len < 0 {
        return alloc_c_string("");
    }
    let a_len = a_len as usize;
    let b_len = b_len as usize;
    let a_bytes = if a.is_null() || a_len == 0 {
        &[][..]
    } else {
        // SAFETY: caller guarantees `a` is valid for `a_len` bytes.
        unsafe { std::slice::from_raw_parts(a as *const u8, a_len) }
    };
    let b_bytes = if b.is_null() || b_len == 0 {
        &[][..]
    } else {
        // SAFETY: caller guarantees `b` is valid for `b_len` bytes.
        unsafe { std::slice::from_raw_parts(b as *const u8, b_len) }
    };
    let mut result = Vec::with_capacity(a_len + b_len);
    result.extend_from_slice(a_bytes);
    result.extend_from_slice(b_bytes);
    alloc_c_string_from_bytes(&result)
}

/// Deep-eval 2026-08-09 (test_result_match parity): the current OS error as
/// a heap C string, formatted like Rust's `io::Error` Display
/// ("No such file or directory (os error 2)"). Used by the native
/// read_file/write_file Err paths so their messages match the interpreter's
/// `e.to_string()` instead of a hard-coded string.
///
/// # Safety
/// Pointer arguments must be valid NUL-terminated C strings (unless
/// documented otherwise), live Mimi list pointers from `mimi_list_*` calls,
/// and key/value arrays must have at least `len` valid elements.
#[no_mangle]
pub unsafe extern "C" fn mimi_os_error_message() -> *mut std::ffi::c_char {
    let msg = std::io::Error::last_os_error().to_string();
    alloc_c_string(&msg)
}

/// Character-index (Unicode scalar) `char_at`.
/// Returns a new heap-allocated 1-char string; aborts on OOB / invalid UTF-8.
///
/// # Safety
/// Pointer arguments must be valid NUL-terminated C strings (unless
/// documented otherwise), live Mimi list pointers from `mimi_list_*` calls,
/// and key/value arrays must have at least `len` valid elements.
#[no_mangle]
pub unsafe extern "C" fn mimi_str_char_at(
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

/// Length-aware character-index (Unicode scalar) `char_at`.
/// Uses the explicit byte length so embedded NUL bytes are preserved
/// (batch4/02 P1-1). Aborts on OOB / invalid UTF-8.
///
/// # Safety
/// Pointers must be valid for the corresponding byte length; negative
/// lengths are rejected.
#[no_mangle]
pub unsafe extern "C" fn mimi_str_char_at_ll(
    s: *const std::ffi::c_char,
    s_len: i64,
    index: i64,
) -> *mut std::ffi::c_char {
    if s_len < 0 || index < 0 {
        mimi_runtime_abort(
            b"str_char_at: index out of bounds\0".as_ptr() as *const std::ffi::c_char
        );
    }
    let bytes = if s.is_null() || s_len == 0 {
        &[][..]
    } else {
        // SAFETY: caller guarantees `s` is valid for `s_len` bytes.
        unsafe { std::slice::from_raw_parts(s as *const u8, s_len as usize) }
    };
    let text = match std::str::from_utf8(bytes) {
        Ok(t) => t,
        Err(_) => {
            mimi_runtime_abort(b"str_char_at: invalid UTF-8\0".as_ptr() as *const std::ffi::c_char);
        }
    };
    match text.chars().nth(index as usize) {
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
///
/// # Safety
/// Pointer arguments must be valid NUL-terminated C strings (unless
/// documented otherwise), live Mimi list pointers from `mimi_list_*` calls,
/// and key/value arrays must have at least `len` valid elements.
#[no_mangle]
pub unsafe extern "C" fn mimi_str_substring(
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

/// audit-wave1 helper: read a `(ptr, len)` string ABI into a Rust String
/// (lossy UTF-8). Same trust model as `mimi_str_clone`: the caller (codegen)
/// guarantees `ptr` is valid for `len` readable bytes. Negative or absurd
/// lengths are rejected loud; null ptr reads as empty.
fn str_from_ptr_len(ptr: *const std::ffi::c_char, len: i64) -> String {
    if ptr.is_null() || len <= 0 {
        return String::new();
    }
    // RT-H9 parity with mimi_str_clone: cap absurd allocation/read requests.
    const MAX_STR_LEN: i64 = 64 * 1024 * 1024; // 64 MiB
    if len > MAX_STR_LEN {
        unsafe {
            mimi_runtime_abort(
                b"string builtin: length out of bounds\0".as_ptr() as *const std::ffi::c_char
            );
        }
    }
    // SAFETY: caller guarantees `ptr` points to at least `len` readable bytes
    // (ptr+len string ABI used by codegen, same contract as mimi_str_clone);
    // `len` was bounds-checked above.
    let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
    String::from_utf8_lossy(bytes).into_owned()
}

/// audit-wave1 (VM function-form parity, interp builtins/string.rs
/// builtin_str_substring): substring `[start, end)` with indices CLAMPED to
/// the char count. Aborts only if `start > end` AFTER clamping. The method
/// form `mimi_str_substring` above stays strict — do not conflate them.
///
/// # Safety
/// Pointer arguments must be valid NUL-terminated C strings (unless
/// documented otherwise), live Mimi list pointers from `mimi_list_*` calls,
/// and key/value arrays must have at least `len` valid elements.
#[no_mangle]
pub unsafe extern "C" fn mimi_str_substring_clamp(
    ptr: *const std::ffi::c_char,
    len: i64,
    start: i64,
    end: i64,
) -> *mut std::ffi::c_char {
    let ss = str_from_ptr_len(ptr, len);
    let chars: Vec<char> = ss.chars().collect();
    let n = chars.len();
    // VM parity: negative indices wrap through `as usize` and saturate at n.
    let si = (start as usize).min(n);
    let ei = (end as usize).min(n);
    if si > ei {
        mimi_runtime_abort(b"str_substring: start > end\0".as_ptr() as *const std::ffi::c_char);
    }
    let result: String = chars[si..ei].iter().collect();
    alloc_c_string(&result)
}

/// audit-wave1: Unicode-correct full-string case conversion (VM parity:
/// `s.to_uppercase()`), replacing codegen's byte-wise emulation. Returns a
/// freshly heap-allocated string (caller frees via mimi_string_free).
///
/// # Safety
/// Pointer arguments must be valid NUL-terminated C strings (unless
/// documented otherwise), live Mimi list pointers from `mimi_list_*` calls,
/// and key/value arrays must have at least `len` valid elements.
#[no_mangle]
pub unsafe extern "C" fn mimi_str_to_upper(
    ptr: *const std::ffi::c_char,
    len: i64,
) -> *mut std::ffi::c_char {
    let ss = str_from_ptr_len(ptr, len);
    alloc_c_string(&ss.to_uppercase())
}

/// audit-wave1: Unicode-correct full-string case conversion (VM parity:
/// `s.to_lowercase()`). Returns a freshly heap-allocated string.
///
/// # Safety
/// Pointer arguments must be valid NUL-terminated C strings (unless
/// documented otherwise), live Mimi list pointers from `mimi_list_*` calls,
/// and key/value arrays must have at least `len` valid elements.
#[no_mangle]
pub unsafe extern "C" fn mimi_str_to_lower(
    ptr: *const std::ffi::c_char,
    len: i64,
) -> *mut std::ffi::c_char {
    let ss = str_from_ptr_len(ptr, len);
    alloc_c_string(&ss.to_lowercase())
}

/// audit-wave1: Unicode-aware trim (VM parity: Rust `str::trim`, which strips
/// all chars with the White_Space property). Returns a freshly heap-allocated
/// string.
///
/// # Safety
/// Pointer arguments must be valid NUL-terminated C strings (unless
/// documented otherwise), live Mimi list pointers from `mimi_list_*` calls,
/// and key/value arrays must have at least `len` valid elements.
#[no_mangle]
pub unsafe extern "C" fn mimi_str_trim(
    ptr: *const std::ffi::c_char,
    len: i64,
) -> *mut std::ffi::c_char {
    let ss = str_from_ptr_len(ptr, len);
    alloc_c_string(ss.trim())
}

///
/// # Safety
/// Pointer arguments must be valid NUL-terminated C strings (unless
/// documented otherwise), live Mimi list pointers from `mimi_list_*` calls,
/// and key/value arrays must have at least `len` valid elements.
#[no_mangle]
pub unsafe extern "C" fn mimi_str_split(
    s: *const std::ffi::c_char,
    delim: *const std::ffi::c_char,
) -> *mut MimiList {
    // Legacy C-string entry: input is NUL-terminated. The production path
    // is `mimi_str_split_ll` (ptr+len). Elements are still fat boxes.
    let ss = unsafe { cstr_to_string(s) };
    let d = unsafe { cstr_to_string(delim) };
    unsafe {
        list_string::mimi_str_split_ll(
            ss.as_ptr() as *const std::ffi::c_char,
            ss.len() as i64,
            d.as_ptr() as *const std::ffi::c_char,
            d.len() as i64,
        )
    }
}

///
/// # Safety
/// Pointer arguments must be valid NUL-terminated C strings (unless
/// documented otherwise), live Mimi list pointers from `mimi_list_*` calls,
/// and key/value arrays must have at least `len` valid elements.
#[no_mangle]
pub unsafe extern "C" fn mimi_str_join_ll(
    list: *const MimiList,
    sep: *const std::ffi::c_char,
    sep_len: i64,
    out_len: *mut i64,
) -> *mut std::ffi::c_char {
    if sep.is_null() || sep_len < 0 {
        if !out_len.is_null() {
            unsafe { *out_len = 0 };
        }
        return alloc_c_string("");
    }
    let sep_bytes = unsafe { std::slice::from_raw_parts(sep as *const u8, sep_len as usize) };
    unsafe { list_string::join_fat_string_list(list, sep_bytes, out_len) }
}

#[no_mangle]
pub unsafe extern "C" fn mimi_str_join(
    list: *const MimiList,
    sep: *const std::ffi::c_char,
) -> *mut std::ffi::c_char {
    if unsafe { list_string::list_has_legacy_string_abi(list) } {
        return std::ptr::null_mut();
    }
    let separator = unsafe { cstr_to_string(sep) };
    let mut out_len: i64 = 0;
    unsafe { list_string::join_fat_string_list(list, separator.as_bytes(), &mut out_len) }
}

/// Render a `MimiList` (codegen `{i64 len, i8* data}`) to a printable
/// heap-allocated C string. Used by the codegen `to_string` builtin
/// when it encounters a list value.
///
/// This ABI entry point is string-list-specific. Native codegen passes the
/// stable two-field `{len, data}` list layout, which has no `element_kind`
/// tail field. Numeric and structured lists use their dedicated formatters.
///
/// # Safety
/// `list` must be null or a live `MimiList` pointer created by the
/// runtime (or a legal codegen list prefix where documented).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_to_string(list: *const MimiList) -> *mut std::ffi::c_char {
    if list.is_null() {
        return alloc_c_string("[]");
    }
    // SAFETY: both runtime MimiList and native codegen lists begin with the
    // repr(C) `{len, data}` prefix, and the null case was handled above.
    let lst = unsafe { &*list.cast::<MimiListAbiPrefix>() };
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
        // SAFETY: `lst` points to a valid, properly aligned value
        // `lst.data` is `*mut *mut c_char`; dereference to a C string.
        let item_ptr = unsafe { *lst.data.offset(i) };
        if item_ptr.is_null() {
            parts.push(String::from("null"));
        } else {
            match unsafe { list_string::read_mimi_str(item_ptr) } {
                Ok((ptr, len)) => parts.push(str_from_ptr_len(ptr, len)),
                Err(_) => parts.push(unsafe { cstr_to_string(item_ptr) }),
            }
        }
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

/// Render `List<Result<i32,i32>>` (ptrtoint of Result structs) as JSON array of
/// `{"Ok":[n]}` / `{"Err":[n]}` tags matching interp to_json.
///
/// # Safety
/// `list` must be null or a live `MimiList` pointer created by the
/// runtime (or a legal codegen list prefix where documented).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_result_i64_to_json(
    list: *const MimiList,
) -> *mut std::ffi::c_char {
    list_result_to_json_impl(list, 0)
}

/// Render `List<Result<Map<string, V>, i32>>` as JSON array.
/// `mode`: 0=i64 map, 1=string map, 2=bool map, 3=f64 map (same as other map JSON helpers).
///
/// # Safety
/// `list` must be null or a live `MimiList` pointer created by the
/// runtime (or a legal codegen list prefix where documented).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_result_map_to_json(
    list: *const MimiList,
    mode: i64,
) -> *mut std::ffi::c_char {
    list_result_to_json_impl(list, mode + 10)
}

/// True if every page covering `[addr, addr+len)` is currently mapped.
/// Pattern lifted from `safe_c_string_from_handle`: mincore-probe BEFORE any
/// read so hostile/garbage pointers can never drive a SIGSEGV.
fn memory_range_mapped(addr: usize, len: usize) -> bool {
    if len == 0 {
        return true;
    }
    // SAFETY: `libc::sysconf` is an async-signal-safe POSIX function.
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
    let page_size = if page_size == 0 { 4096 } else { page_size };
    let end = match addr.checked_add(len) {
        Some(e) => e,
        None => return false,
    };
    let page_start = (addr / page_size) * page_size;
    let span = end.saturating_sub(page_start);
    let n_pages = span.div_ceil(page_size);
    let mut vec = vec![0u8; n_pages];
    // SAFETY: `page_start` is page-aligned and `n_pages * page_size >= span`
    // covers the queried range; vec is a valid buffer of exactly one byte per
    // page (mincore writes one status byte per page).
    unsafe { libc::mincore(page_start as *mut std::ffi::c_void, span, vec.as_mut_ptr()) == 0 }
}

/// Decode Result Err payload to a JSON string fragment (already escaped/quoted).
fn decode_result_err_string(err: i64) -> String {
    const MIN_HEAP: i64 = 1_048_576;
    if err >= MIN_HEAP && (err as u64) % 8 == 0 {
        // Prefer Mimi string struct {ptr, i64} heap layout.
        let base = err as usize;
        // audit-wave1 (audit §10 HIGH): validate BOTH the struct page(s) AND
        // the target byte range before dereferencing. The old code only
        // mincore-probed the base page, then dereferenced the inner {ptr,len}
        // blind → OOB read / SIGSEGV on garbage payloads.
        if memory_range_mapped(base, 16) {
            let base_ptr = base as *const u8;
            // SAFETY: memory_range_mapped confirmed both i64 slots are on
            // mapped pages; `err` is 8-aligned (checked above) so both loads
            // are aligned.
            let ptr = unsafe { *(base_ptr as *const *const u8) };
            let len = unsafe { *(base_ptr.add(8) as *const i64) };
            if !ptr.is_null() && (0..1_000_000).contains(&len) {
                // Second validation gate: probe the TARGET range the struct
                // claims to point at. Unmapped/overflowing → sentinel empty
                // string, never SIGSEGV or an info-read of foreign memory.
                if memory_range_mapped(ptr as usize, len as usize) {
                    // SAFETY: target range confirmed mapped above; len bounds
                    // checked (0..1_000_000); u8 slice has alignment 1.
                    let slice = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
                    if let Ok(s) = std::str::from_utf8(slice) {
                        return json_escape_string(s);
                    }
                }
                // Invalid inner target / non-UTF-8: sentinel empty string.
                return json_escape_string("");
            }
            // {ptr,len} implausible — not a string struct. Fallback: bounded
            // C-string decode at the err address itself (mincore-checked
            // internally by safe_c_string_from_handle).
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
    // SAFETY: `list` is non-null and points to a valid `MimiList`
    let lst = unsafe { &*list.cast::<MimiListAbiPrefix>() };
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
        // SAFETY: `lst.data` points to a valid, properly aligned value
        let base = unsafe { *(lst.data as *const i64).offset(i) } as *const u8;
        if base.is_null() {
            parts.push(String::from("null"));
            continue;
        }
        // Layout {i1 disc, i64 ok, i64 err} — disc at 0, ok at 8, err at 16 on x86_64.
        if !pages_mapped(base as usize, 24) {
            // Malformed/stale result pack: fail closed instead of reading
            // unmapped memory (P1-21).
            parts.push(String::from("null"));
            continue;
        }
        // SAFETY: pages_mapped confirmed the 24-byte result pack is resident.
        let disc = unsafe { *base };
        let ok = unsafe { *(base.add(8) as *const i64) };
        let err = unsafe { *(base.add(16) as *const i64) };
        if disc != 0 {
            if mode >= 20 {
                // Product Map: mode = 20 + arity.
                let arity = mode - 20;
                let json_ptr = unsafe { mimi_map_to_json_product_i64(ok as MapHandle, arity, 0) };
                // SAFETY: `json_ptr` is a heap-allocated C string (or heap block) that was returned by a prior allocation; `mimi_free` is the matching deallocation (mimi_alloc/alloc_c_string path)
                let s = unsafe { cstr_to_string(json_ptr) };
                if !json_ptr.is_null() {
                    // SAFETY: `mut` points to a valid, properly aligned value
                    mimi_free(json_ptr as *mut std::ffi::c_void);
                }
                parts.push(format!("{{\"Ok\":[{}]}}", s));
            } else if mode >= 10 {
                let map_mode = mode - 10;
                let json_ptr = match map_mode {
                    1 => unsafe { mimi_map_to_json_string(ok as MapHandle) },
                    2 => unsafe { mimi_map_to_json_bool(ok as MapHandle) },
                    3 => unsafe { mimi_map_to_json_f64_serde(ok as MapHandle) },
                    _ => unsafe { mimi_map_to_json_i64(ok as MapHandle) },
                };
                // SAFETY: `json_ptr` is a heap-allocated C string (or heap block) that was returned by a prior allocation; `mimi_free` is the matching deallocation (mimi_alloc/alloc_c_string path)
                let s = unsafe { cstr_to_string(json_ptr) };
                if !json_ptr.is_null() {
                    // SAFETY: `mut` points to a valid, properly aligned value
                    mimi_free(json_ptr as *mut std::ffi::c_void);
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
///
/// # Safety
/// `list` must be null or a live `MimiList` pointer created by the
/// runtime (or a legal codegen list prefix where documented).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_option_map_to_json(
    list: *const MimiList,
    mode: i64,
) -> *mut std::ffi::c_char {
    if list.is_null() {
        return alloc_c_string("[]");
    }
    // SAFETY: `list` is non-null and points to a valid `MimiList`
    let lst = unsafe { &*list.cast::<MimiListAbiPrefix>() };
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
        // SAFETY: `lst.data` points to a valid, properly aligned value
        let base = unsafe { *(lst.data as *const i64).offset(i) } as *const u8;
        if base.is_null() {
            parts.push(String::from("\"None\""));
            continue;
        }
        // SAFETY: heap Option {i1, i64 map handle}.
        if !pages_mapped(base as usize, 16) {
            // Malformed/stale option pack: fail closed instead of reading
            // unmapped memory (P1-21).
            parts.push(String::from("\"None\""));
            continue;
        }
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
                    1 => unsafe { mimi_map_to_json_string(handle) },
                    2 => unsafe { mimi_map_to_json_bool(handle) },
                    3 => unsafe { mimi_map_to_json_f64_serde(handle) },
                    _ => unsafe { mimi_map_to_json_i64(handle) },
                }
            };
            // SAFETY: `json_ptr` is a heap-allocated C string (or heap block) that was returned by a prior allocation; `mimi_free` is the matching deallocation (mimi_alloc/alloc_c_string path)
            let s = unsafe { cstr_to_string(json_ptr) };
            if !json_ptr.is_null() {
                // SAFETY: `mut` points to a valid, properly aligned value
                mimi_free(json_ptr as *mut std::ffi::c_void);
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
///
/// # Safety
/// `list` must be null or a live `MimiList` pointer created by the
/// runtime (or a legal codegen list prefix where documented).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_option_i64_to_json(
    list: *const MimiList,
) -> *mut std::ffi::c_char {
    if list.is_null() {
        return alloc_c_string("[]");
    }
    // SAFETY: `list` is non-null and points to a valid `MimiList`
    let lst = unsafe { &*list.cast::<MimiListAbiPrefix>() };
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
        // SAFETY: `lst.data` points to a valid, properly aligned value
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
///
/// # Safety
/// `list` must be null or a live `MimiList` pointer created by the
/// runtime (or a legal codegen list prefix where documented).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_map_to_string(list: *const MimiList) -> *mut std::ffi::c_char {
    list_map_to_string_impl(list, MapJsonMode::Int, ", ")
}

/// List of Map for to_json with string values (no space after comma).
///
/// # Safety
/// `list` must be null or a live `MimiList` pointer created by the
/// runtime (or a legal codegen list prefix where documented).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_map_to_json_string(
    list: *const MimiList,
) -> *mut std::ffi::c_char {
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
    // SAFETY: `list` is non-null and points to a valid `MimiList`
    let lst = unsafe { &*list.cast::<MimiListAbiPrefix>() };
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
        // SAFETY: `lst.data` points to a valid, properly aligned value
        let handle = unsafe { *(lst.data as *const i64).offset(i) } as MapHandle;
        let json_ptr = match mode {
            MapJsonMode::String => unsafe { mimi_map_to_json_string(handle) },
            MapJsonMode::Bool => unsafe { mimi_map_to_json_bool(handle) },
            MapJsonMode::Float | MapJsonMode::FloatJson => unsafe {
                mimi_map_to_json_f64_serde(handle)
            },
            MapJsonMode::Int => unsafe { mimi_map_to_json_i64(handle) },
        };
        // SAFETY: `json_ptr` is a heap-allocated C string (or heap block) that was returned by a prior allocation; `mimi_free` is the matching deallocation (mimi_alloc/alloc_c_string path)
        let s = unsafe { cstr_to_string(json_ptr) };
        if !json_ptr.is_null() {
            // SAFETY: `mut` points to a valid, properly aligned value
            mimi_free(json_ptr as *mut std::ffi::c_void);
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
        unsafe { mimi_map_to_json_map_product_i64(handle, mode - 40, 0) }
    } else if mode >= 30 {
        unsafe { mimi_map_to_json_set_product_i64(handle, mode - 30, 0) }
    } else if mode >= 20 {
        unsafe { mimi_map_to_json_list_product_i64(handle, mode - 20, 0) }
    } else if mode >= 10 {
        unsafe { mimi_map_to_json_product_i64(handle, mode - 10, 0) }
    } else {
        match mode {
            1 => unsafe { mimi_map_to_json_string(handle) },
            2 => unsafe { mimi_map_to_json_bool(handle) },
            3 => unsafe { mimi_map_to_json_f64_serde(handle) },
            _ => unsafe { mimi_map_to_json_i64(handle) },
        }
    };
    // SAFETY: `json_ptr` is a heap-allocated C string (or heap block) that was returned by a prior allocation; `mimi_free` is the matching deallocation (mimi_alloc/alloc_c_string path)
    let s = unsafe { cstr_to_string(json_ptr) };
    if !json_ptr.is_null() {
        // SAFETY: `mut` points to a valid, properly aligned value
        mimi_free(json_ptr as *mut std::ffi::c_void);
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
        unsafe { mimi_set_to_json_map_product_i64(handle, mode - 70, 0) }
    } else if mode >= 10 {
        unsafe { mimi_set_to_json_product_i64(handle, mode - 10, 0) }
    } else {
        match mode {
            1 => unsafe { mimi_set_to_json_string(handle) },
            2 => unsafe { mimi_set_to_json_bool(handle) },
            3 => unsafe { mimi_set_to_json_f64(handle) },
            _ => unsafe { mimi_set_to_json_i64(handle) },
        }
    };
    // SAFETY: `json_ptr` is a heap-allocated C string (or heap block) that was returned by a prior allocation; `mimi_free` is the matching deallocation (mimi_alloc/alloc_c_string path)
    let s = unsafe { cstr_to_string(json_ptr) };
    if !json_ptr.is_null() {
        // SAFETY: `mut` points to a valid, properly aligned value
        mimi_free(json_ptr as *mut std::ffi::c_void);
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
            unsafe { mimi_map_to_json_result_product_i64(ok_handle, mode - 60, 0) }
        } else if mode >= 50 {
            unsafe { mimi_map_to_json_option_product_i64(ok_handle, mode - 50, 0) }
        } else if mode >= 40 {
            unsafe { mimi_map_to_json_map_product_i64(ok_handle, mode - 40, 0) }
        } else if mode >= 30 {
            unsafe { mimi_map_to_json_set_product_i64(ok_handle, mode - 30, 0) }
        } else if mode >= 20 {
            unsafe { mimi_map_to_json_list_product_i64(ok_handle, mode - 20, 0) }
        } else if mode >= 10 {
            unsafe { mimi_map_to_json_product_i64(ok_handle, mode - 10, 0) }
        } else {
            match mode {
                1 => unsafe { mimi_map_to_json_string(ok_handle) },
                2 => unsafe { mimi_map_to_json_bool(ok_handle) },
                3 => unsafe { mimi_map_to_json_f64_serde(ok_handle) },
                _ => unsafe { mimi_map_to_json_i64(ok_handle) },
            }
        };
        // SAFETY: `json_ptr` is a heap-allocated C string (or heap block) that was returned by a prior allocation; `mimi_free` is the matching deallocation (mimi_alloc/alloc_c_string path)
        let s = unsafe { cstr_to_string(json_ptr) };
        if !json_ptr.is_null() {
            // SAFETY: `mut` points to a valid, properly aligned value
            mimi_free(json_ptr as *mut std::ffi::c_void);
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
            unsafe { mimi_set_to_json_map_product_i64(ok_handle, mode - 70, 0) }
        } else if mode >= 50 {
            unsafe { mimi_set_to_json_option_product_i64(ok_handle, mode - 50, 0) }
        } else if mode >= 10 {
            unsafe { mimi_set_to_json_product_i64(ok_handle, mode - 10, 0) }
        } else {
            match mode {
                1 => unsafe { mimi_set_to_json_string(ok_handle) },
                2 => unsafe { mimi_set_to_json_bool(ok_handle) },
                3 => unsafe { mimi_set_to_json_f64(ok_handle) },
                _ => unsafe { mimi_set_to_json_i64(ok_handle) },
            }
        };
        // SAFETY: `json_ptr` is a heap-allocated C string (or heap block) that was returned by a prior allocation; `mimi_free` is the matching deallocation (mimi_alloc/alloc_c_string path)
        let s = unsafe { cstr_to_string(json_ptr) };
        if !json_ptr.is_null() {
            // SAFETY: `mut` points to a valid, properly aligned value
            mimi_free(json_ptr as *mut std::ffi::c_void);
        }
        alloc_c_string(&format!("{{\"Ok\":[{}]}}", s))
    } else {
        let err_s = decode_result_err_string(err);
        alloc_c_string(&format!("{{\"Err\":[{}]}}", err_s))
    }
}

/// Render `List<Set>` as a JSON array of JSON arrays `[[1,2],[3]]`.
///
/// # Safety
/// `list` must be null or a live `MimiList` pointer created by the
/// runtime (or a legal codegen list prefix where documented).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_set_to_json(list: *const MimiList) -> *mut std::ffi::c_char {
    if list.is_null() {
        return alloc_c_string("[]");
    }
    // SAFETY: `list` is non-null and points to a valid `MimiList`
    let lst = unsafe { &*list.cast::<MimiListAbiPrefix>() };
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
        // SAFETY: `lst.data` points to a valid, properly aligned value
        let handle = unsafe { *(lst.data as *const i64).offset(i) } as SetHandle;
        let json_ptr = mimi_set_to_json_i64(handle);
        // SAFETY: `json_ptr` is a valid null-terminated C string returned by a Mimi allocation function
        let s = unsafe { cstr_to_string(json_ptr) };
        if !json_ptr.is_null() {
            // SAFETY: `mut` points to a valid, properly aligned value
            mimi_free(json_ptr as *mut std::ffi::c_void);
        }
        parts.push(s);
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

/// Render `List<Set<product>>` as JSON array of product-set JSON arrays.
///
/// # Safety
/// `list` must be null or a live `MimiList` pointer created by the
/// runtime (or a legal codegen list prefix where documented).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_set_product_to_json(
    list: *const MimiList,
    arity: i64,
) -> *mut std::ffi::c_char {
    if list.is_null() || arity <= 0 {
        return alloc_c_string("[]");
    }
    // SAFETY: `list` is non-null and points to a valid `MimiList`
    let lst = unsafe { &*list.cast::<MimiListAbiPrefix>() };
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
        // SAFETY: `lst.data` points to a valid, properly aligned value
        let handle = unsafe { *(lst.data as *const i64).offset(i) } as SetHandle;
        let json_ptr = mimi_set_to_json_product_i64(handle, arity, 0);
        // SAFETY: `json_ptr` is a valid null-terminated C string returned by a Mimi allocation function
        let s = unsafe { cstr_to_string(json_ptr) };
        if !json_ptr.is_null() {
            // SAFETY: `mut` points to a valid, properly aligned value
            mimi_free(json_ptr as *mut std::ffi::c_void);
        }
        parts.push(s);
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

/// Render `List<Set<product>>` Display as `[Set{(1, 2)}, ...]`.
///
/// # Safety
/// `list` must be null or a live `MimiList` pointer created by the
/// runtime (or a legal codegen list prefix where documented).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_set_product_to_string(
    list: *const MimiList,
    arity: i64,
) -> *mut std::ffi::c_char {
    if list.is_null() || arity <= 0 {
        return alloc_c_string("[]");
    }
    // SAFETY: `list` is non-null and points to a valid `MimiList`
    let lst = unsafe { &*list.cast::<MimiListAbiPrefix>() };
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
        // SAFETY: `lst.data` points to a valid, properly aligned value
        let handle = unsafe { *(lst.data as *const i64).offset(i) } as SetHandle;
        let disp = mimi_set_to_json_product_i64(handle, arity, 1);
        // SAFETY: `disp` is a valid null-terminated C string returned by a Mimi allocation function
        let s = unsafe { cstr_to_string(disp) };
        if !disp.is_null() {
            // SAFETY: `mut` points to a valid, properly aligned value
            mimi_free(disp as *mut std::ffi::c_void);
        }
        parts.push(s);
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

/// Render `List<Set>` (i64 set handles) as `[Set{1, 2}, ...]`.
///
/// # Safety
/// `list` must be null or a live `MimiList` pointer created by the
/// runtime (or a legal codegen list prefix where documented).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_set_to_string(list: *const MimiList) -> *mut std::ffi::c_char {
    if list.is_null() {
        return alloc_c_string("[]");
    }
    // SAFETY: `list` is non-null and points to a valid `MimiList`
    let lst = unsafe { &*list.cast::<MimiListAbiPrefix>() };
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
        // SAFETY: `lst.data` points to a valid, properly aligned value
        let handle = unsafe { *(lst.data as *const i64).offset(i) } as SetHandle;
        let disp = unsafe { mimi_set_to_display(handle) };
        // SAFETY: `disp` is a valid null-terminated C string returned by a Mimi allocation function
        let s = unsafe { cstr_to_string(disp) };
        if !disp.is_null() {
            // SAFETY: `mut` points to a valid, properly aligned value
            mimi_free(disp as *mut std::ffi::c_void);
        }
        parts.push(s);
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

/// Render a codegen `List<i32>` (layout `{i64 len, i8* data}` where data points
/// to pointer-sized slots) to a printable heap-allocated C string.
///
/// # Safety
/// `list` must be null or a live `MimiList` pointer created by the
/// runtime (or a legal codegen list prefix where documented).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_i32_to_string(list: *const MimiList) -> *mut std::ffi::c_char {
    if list.is_null() {
        return alloc_c_string("[]");
    }
    // SAFETY: `list` is non-null and points to a valid `MimiList`
    let lst = unsafe { &*list.cast::<MimiListAbiPrefix>() };
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
        // SAFETY: `lst.data` points to a valid, properly aligned value
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
///
/// # Safety
/// `list` must be null or a live `MimiList` pointer created by the
/// runtime (or a legal codegen list prefix where documented).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_bool_to_json(list: *const MimiList) -> *mut std::ffi::c_char {
    if list.is_null() {
        return alloc_c_string("[]");
    }
    // SAFETY: `list` is non-null and points to a valid `MimiList`
    let lst = unsafe { &*list.cast::<MimiListAbiPrefix>() };
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
        // SAFETY: `lst.data` points to a valid, properly aligned value
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
///
/// # Safety
/// `list` must be null or a live `MimiList` pointer created by the
/// runtime (or a legal codegen list prefix where documented).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_i64_to_json(list: *const MimiList) -> *mut std::ffi::c_char {
    if list.is_null() {
        return alloc_c_string("[]");
    }
    // SAFETY: `list` is non-null and points to a valid `MimiList`
    let lst = unsafe { &*list.cast::<MimiListAbiPrefix>() };
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
        // SAFETY: `lst.data` points to a valid, properly aligned value
        let item = unsafe { *(lst.data as *const i64).offset(i) };
        parts.push(item.to_string());
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

/// Render a codegen `List<f64>` (layout `{i64 len, i8* data}` where data points
/// to i64 slots containing bitcast f64 values) to a JSON array string.
/// Returns a heap-allocated C string.
///
/// # Safety
/// `list` must be null or a live `MimiList` pointer created by the
/// runtime (or a legal codegen list prefix where documented).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_f64_to_json(list: *const MimiList) -> *mut std::ffi::c_char {
    if list.is_null() {
        return alloc_c_string("[]");
    }
    // SAFETY: `list` is non-null and points to a valid `MimiList`
    let lst = unsafe { &*list.cast::<MimiListAbiPrefix>() };
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
        // SAFETY: `lst.data` points to a valid, properly aligned value
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
///
/// # Safety
/// `list` must be null or a live `MimiList` pointer created by the
/// runtime (or a legal codegen list prefix where documented).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_str_to_json(list: *const MimiList) -> *mut std::ffi::c_char {
    if list.is_null() {
        return alloc_c_string("[]");
    }
    // SAFETY: `list` is non-null and points to a valid `MimiList`
    let lst = unsafe { &*list.cast::<MimiListAbiPrefix>() };
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
        // SAFETY: `lst.data` points to a valid, properly aligned value
        let item_ptr = unsafe { *(lst.data as *const *mut std::ffi::c_char).offset(i) };
        if item_ptr.is_null() {
            parts.push(String::from("null"));
        } else {
            let s = match unsafe { list_string::read_mimi_str(item_ptr) } {
                Ok((ptr, len)) => str_from_ptr_len(ptr, len),
                Err(_) => unsafe { cstr_to_string(item_ptr) },
            };
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
///
/// # Safety
/// `list` must be null or a live `MimiList` pointer created by the
/// runtime (or a legal codegen list prefix where documented).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_list_to_string(
    list: *const MimiList,
    elem_to_string: extern "C" fn(*const MimiList) -> *mut std::ffi::c_char,
) -> *mut std::ffi::c_char {
    list_list_to_string_impl(list, elem_to_string, ", ")
}

/// Compact JSON form of `List<List<T>>` (no spaces after commas).
///
/// # Safety
/// `list` must be null or a live `MimiList` pointer created by the
/// runtime (or a legal codegen list prefix where documented).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_list_to_json(
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
    let lst = unsafe { &*list.cast::<MimiListAbiPrefix>() };
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
        // SAFETY: `lst.data` points to a valid, properly aligned value
        // `lst.data` points to inner list pointers (`*const MimiList`) or
        // ptrtoint handles stored as i64 slots.
        let slot = unsafe { *(lst.data as *const i64).offset(i) };
        let inner = slot as *const MimiList;
        if inner.is_null() || slot == 0 {
            parts.push(String::from("null"));
        } else {
            let inner_str = elem_to_string(inner);
            // SAFETY: `inner_str` is a heap-allocated C string (or heap block) that was returned by a prior allocation; `mimi_free` is the matching deallocation (mimi_alloc/alloc_c_string path)
            let s = unsafe { cstr_to_string(inner_str) };
            // The inner formatter returns a heap-allocated string that we now own.
            if !inner_str.is_null() {
                // SAFETY: `inner_str` was allocated by `alloc_c_string` in the inner formatter.
                mimi_free(inner_str as *mut std::ffi::c_void);
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
///
/// # Safety
/// `list` must be null or a live `MimiList` pointer created by the
/// runtime (or a legal codegen list prefix where documented).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_record_to_json(
    list: *const MimiList,
    elem_to_json: extern "C" fn(*const std::ffi::c_void) -> *mut std::ffi::c_char,
) -> *mut std::ffi::c_char {
    if list.is_null() {
        return alloc_c_string("[]");
    }
    // SAFETY: caller ensures `list` is a valid `*const MimiList` or null.
    let lst = unsafe { &*list.cast::<MimiListAbiPrefix>() };
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
        // SAFETY: `lst.data` points to a valid, properly aligned value
        let elem_ptr = unsafe { *(lst.data as *const *const std::ffi::c_void).offset(i) };
        if elem_ptr.is_null() {
            parts.push(String::from("null"));
        } else {
            let elem_json = elem_to_json(elem_ptr);
            // SAFETY: `elem_json` is a heap-allocated C string (or heap block) that was returned by a prior allocation; `mimi_free` is the matching deallocation (mimi_alloc/alloc_c_string path)
            let s = unsafe { cstr_to_string(elem_json) };
            if !elem_json.is_null() {
                // SAFETY: `elem_json` was allocated by `alloc_c_string` in the callback.
                mimi_free(elem_json as *mut std::ffi::c_void);
            }
            parts.push(s);
        }
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

///
/// # Safety
/// Pointer arguments must be valid NUL-terminated C strings (unless
/// documented otherwise), live Mimi list pointers from `mimi_list_*` calls,
/// and key/value arrays must have at least `len` valid elements.
#[no_mangle]
pub unsafe extern "C" fn mimi_str_replace(
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

/// Length-aware string replacement. Uses explicit byte lengths so embedded
/// NUL bytes are preserved (batch4/02 P1-1). Writes the result byte length
/// to `out_len` when non-null.
///
/// # Safety
/// Pointers must be valid for the corresponding byte lengths; negative
/// lengths are rejected.
#[no_mangle]
pub unsafe extern "C" fn mimi_str_replace_ll(
    s: *const std::ffi::c_char,
    s_len: i64,
    from: *const std::ffi::c_char,
    from_len: i64,
    to: *const std::ffi::c_char,
    to_len: i64,
    out_len: *mut i64,
) -> *mut std::ffi::c_char {
    if !out_len.is_null() {
        // SAFETY: `out_len` was checked non-null above.
        unsafe { *out_len = 0 };
    }
    if s_len < 0 || from_len < 0 || to_len < 0 {
        return alloc_c_string("");
    }
    let hay = if s.is_null() || s_len == 0 {
        &[][..]
    } else {
        // SAFETY: caller guarantees `s` is valid for `s_len` bytes.
        unsafe { std::slice::from_raw_parts(s as *const u8, s_len as usize) }
    };
    let needle = if from.is_null() || from_len == 0 {
        &[][..]
    } else {
        // SAFETY: caller guarantees `from` is valid for `from_len` bytes.
        unsafe { std::slice::from_raw_parts(from as *const u8, from_len as usize) }
    };
    let repl = if to.is_null() || to_len == 0 {
        &[][..]
    } else {
        // SAFETY: caller guarantees `to` is valid for `to_len` bytes.
        unsafe { std::slice::from_raw_parts(to as *const u8, to_len as usize) }
    };
    let result = if needle.is_empty() {
        hay.to_vec()
    } else {
        let mut out = Vec::with_capacity(hay.len());
        let mut i = 0usize;
        while i < hay.len() {
            if hay[i..].starts_with(needle) {
                out.extend_from_slice(repl);
                i += needle.len();
            } else {
                out.push(hay[i]);
                i += 1;
            }
        }
        out
    };
    if !out_len.is_null() {
        // SAFETY: `out_len` was checked non-null above.
        unsafe { *out_len = result.len() as i64 };
    }
    alloc_c_string_from_bytes(&result)
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
///
/// # Safety
/// When `len` is positive, `str` must point to at least `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn mimi_try_exit_str(str: *const std::ffi::c_char, len: i64) -> ! {
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

/// Count non-overlapping occurrences of `sub` in `s` (O(n) single scan).
/// Returns i32 count. Zero heap allocations during scan.
///
/// # Safety
/// Pointer arguments must be valid NUL-terminated C strings (unless
/// documented otherwise), live Mimi list pointers from `mimi_list_*` calls,
/// and key/value arrays must have at least `len` valid elements.
#[no_mangle]
pub unsafe extern "C" fn mimi_str_count_substring(
    s: *const std::ffi::c_char,
    sub: *const std::ffi::c_char,
) -> i32 {
    if s.is_null() || sub.is_null() {
        return 0;
    }
    // SAFETY: `CStr::from_ptr` requires a null-terminated string. Mimi strings
    // are null-terminated at the ABI boundary (see `cstr_to_string`).
    let s_bytes = unsafe { std::ffi::CStr::from_ptr(s) }.to_bytes();
    let sub_bytes = unsafe { std::ffi::CStr::from_ptr(sub) }.to_bytes();
    if sub_bytes.is_empty() {
        return 0;
    }
    let mut count = 0i32;
    let mut i = 0;
    while i + sub_bytes.len() <= s_bytes.len() {
        if s_bytes[i..].starts_with(sub_bytes) {
            count += 1;
            i += sub_bytes.len();
        } else {
            i += 1;
        }
    }
    count
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

#[no_mangle]
pub extern "C" fn mimi_random() -> f64 {
    // Same simple LCG as the bytecode VM (interp/bytecode/builtins/math.rs):
    // time-derived seed -> 53-bit value -> [0, 1). Keeping the algorithm in
    // one shared runtime export prevents the two backends from drifting
    // (batch4-03 P2-8).
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64;
    let val = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407)
        >> 11;
    (val as f64) / ((1u64 << 53) as f64)
}

// ---------------------------------------------------------------------------
// JSON parser (recursive descent, self-contained)
// ---------------------------------------------------------------------------
// This hand parser is intentionally self-contained. Audit closure
// (0.36.87): known serde/json-number AND string-token divergences from the
// original audit are now closed:
//   - leading zeros ("01") are rejected;
//   - overflowed exponents ("1e999") are rejected as non-finite;
//   - literal "inf"/"nan" are never accepted by the number path;
//   - unknown escapes / raw control characters are rejected in both
//     permissive and strict string paths.
// Structural unification on serde remains a Wave-3 architectural goal but is
// no longer an audit-wave2 open divergence marker.

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
                        // audit-wave1: surrogate pairs combined, lone surrogates
                        // and malformed hex fail the parse (serde parity).
                        let (ch, consumed) = json_decode_unicode_escape(self.p, self.pos + 1)?;
                        result.push(ch);
                        self.pos += consumed;
                    }
                    _ => return None,
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
            // RFC 8259: raw control characters (U+0000..U+001F) are only
            // allowed via escapes; reject them here for serde parity.
            if c < 0x20 {
                return None;
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
        // JSON number grammar forbids leading zeros: "01" / "-01" are not
        // valid tokens even though str::parse would accept them. Reject them
        // here so the permissive accessor/from_json path matches serde_json
        // and the strict validator.
        let first_digit_pos = self.pos;
        if self.p[first_digit_pos] == b'0'
            && first_digit_pos + 1 < self.p.len()
            && self.p[first_digit_pos + 1].is_ascii_digit()
        {
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
            // stdlib JSON 与 serde 语义统一（audit 2026-08-07）：serde_json
            // rejects exponents that overflow f64 (1e999 → "number out of
            // range"); `str::parse::<f64>` silently returns inf instead.
            // The bytecode VM validates via serde_json, so the runtime parser
            // must reject non-finite parses too — otherwise json_is_valid
            // diverges between backends (was: VM false / codegen true).
            if !val.is_finite() {
                return None;
            }
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
        // A3 (audit 2026-08-05 wave-2): the old path only did bracket-depth
        // scanning — `{invalid}` balanced braces returned Some, so
        // mimi_from_json did NOT return NULL on malformed input and codegen's
        // NULL guard never fired (VM reference: serde_json rejects it with
        // "parse error"). Strictly validate FIRST (RFC 8259 recursive
        // descent), then return the original text like the VM does.
        if !self.strict_valid_document() {
            return None;
        }
        self.pos = 0;
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

    /// Strict RFC 8259 validation, independent of the permissive
    /// `parse_value`/`parse_object` scanners used by the accessor externs
    /// (json_get_*). Only the `mimi_from_json`/`mimi_is_valid_json` entry
    /// points go through this; accessors keep their permissive behavior to
    /// avoid regressing their (documented) partial-input tolerance.
    ///
    /// A3 root cause: parse_object/parse_array count braces/brackets without
    /// validating member syntax, so `{invalid}` (balanced) was accepted.
    fn strict_valid_document(&mut self) -> bool {
        self.pos = 0;
        if !self.strict_value() {
            return false;
        }
        self.skip_ws();
        self.pos == self.p.len()
    }

    /// Recursive descent value validator. Returns true iff the value at the
    /// current position parses as a strict JSON value (RFC 8259: object keys
    /// must be strings, colons/commas in the right places, numbers with the
    /// RFC grammar, no trailing garbage).
    ///
    /// 0.35.27 (C4): depth-guarded — shares `self.depth` with the permissive
    /// parser so deeply nested `{{{{...}}}}` returns `false` instead of
    /// overflowing the stack (permissive path already had JSON_MAX_DEPTH=64;
    /// the strict path previously recursed unchecked).
    fn strict_value(&mut self) -> bool {
        self.depth += 1;
        if self.depth > JSON_MAX_DEPTH {
            self.depth -= 1;
            return false;
        }
        let result = self.strict_value_inner();
        self.depth -= 1;
        result
    }

    fn strict_value_inner(&mut self) -> bool {
        self.skip_ws();
        match self.peek() {
            b'{' => self.strict_object(),
            b'[' => self.strict_array(),
            b'"' => self.strict_string(),
            b't' => self.strict_literal("true"),
            b'f' => self.strict_literal("false"),
            b'n' => self.strict_literal("null"),
            b'-' | b'0'..=b'9' => self.strict_number(),
            _ => false,
        }
    }

    fn strict_object(&mut self) -> bool {
        // consume '{'
        self.advance();
        self.skip_ws();
        if self.peek() == b'}' {
            self.advance();
            return true;
        }
        loop {
            // object key must be a string
            if !self.strict_string() {
                return false;
            }
            self.skip_ws();
            if self.peek() != b':' {
                return false;
            }
            self.advance(); // consume ':'
            if !self.strict_value() {
                return false;
            }
            self.skip_ws();
            match self.peek() {
                b',' => {
                    self.advance();
                    self.skip_ws();
                }
                b'}' => {
                    self.advance();
                    return true;
                }
                _ => return false,
            }
        }
    }

    fn strict_array(&mut self) -> bool {
        // consume '['
        self.advance();
        self.skip_ws();
        if self.peek() == b']' {
            self.advance();
            return true;
        }
        loop {
            if !self.strict_value() {
                return false;
            }
            self.skip_ws();
            match self.peek() {
                b',' => {
                    self.advance();
                    self.skip_ws();
                }
                b']' => {
                    self.advance();
                    return true;
                }
                _ => return false,
            }
        }
    }

    fn strict_string(&mut self) -> bool {
        if self.peek() != b'"' {
            return false;
        }
        self.advance();
        loop {
            if self.pos >= self.p.len() {
                return false; // unterminated
            }
            match self.p[self.pos] {
                b'"' => {
                    self.advance();
                    return true;
                }
                b'\\' => {
                    self.advance();
                    if self.pos >= self.p.len() {
                        return false; // trailing backslash
                    }
                    // RFC 8259 escapes: " \ / b f n r t uXXXX
                    match self.p[self.pos] {
                        b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => {
                            self.advance();
                        }
                        b'u' => {
                            self.advance();
                            for _ in 0..4 {
                                if self.pos >= self.p.len()
                                    || !(self.p[self.pos].is_ascii_hexdigit())
                                {
                                    return false;
                                }
                                self.advance();
                            }
                        }
                        _ => return false, // invalid escape
                    }
                }
                // control characters < 0x20 are not allowed raw in strings
                c if c < 0x20 => return false,
                _ => self.advance(),
            }
        }
    }

    fn strict_literal(&mut self, lit: &str) -> bool {
        let b = lit.as_bytes();
        if self.pos + b.len() > self.p.len() {
            return false;
        }
        for (i, c) in b.iter().enumerate() {
            if self.p[self.pos + i] != *c {
                return false;
            }
        }
        self.pos += b.len();
        true
    }

    fn strict_number(&mut self) -> bool {
        // RFC 8259 number: -?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][+-]?[0-9]+)?
        let num_start = self.pos;
        if self.peek() == b'-' {
            self.advance();
        }
        if self.peek() == b'0' {
            self.advance();
            // leading zero: next digit is illegal (01, 00, 0123)
            if self.pos < self.p.len() && self.p[self.pos].is_ascii_digit() {
                return false;
            }
        } else if matches!(self.peek(), b'1'..=b'9') {
            while self.pos < self.p.len() && self.p[self.pos].is_ascii_digit() {
                self.advance();
            }
        } else {
            return false; // "-" alone or non-digit
        }
        if self.pos >= self.p.len() {
            return true;
        }
        if self.p[self.pos] == b'.' {
            self.advance();
            let frac_start = self.pos;
            while self.pos < self.p.len() && self.p[self.pos].is_ascii_digit() {
                self.advance();
            }
            if self.pos == frac_start {
                return false; // "." with no digits
            }
        }
        if self.pos < self.p.len() && (self.p[self.pos] == b'e' || self.p[self.pos] == b'E') {
            self.advance();
            if self.pos < self.p.len() && (self.p[self.pos] == b'+' || self.p[self.pos] == b'-') {
                self.advance();
            }
            let exp_start = self.pos;
            while self.pos < self.p.len() && self.p[self.pos].is_ascii_digit() {
                self.advance();
            }
            if self.pos == exp_start {
                return false; // "e" with no digits
            }
        }
        // stdlib JSON 与 serde 语义统一（audit 2026-08-07）：serde_json
        // rejects float literals whose value overflows f64 ("number out of
        // range": 1e999, -1e999); the bytecode VM validates via serde_json,
        // so the grammar-only scan must add the same range check for tokens
        // containing '.' or 'e'. Huge INTEGERS stay valid (serde_json parses
        // them via arbitrary precision).
        let tok = &self.p[num_start..self.pos];
        if tok.contains(&b'.') || tok.contains(&b'e') || tok.contains(&b'E') {
            if let Ok(s) = std::str::from_utf8(tok) {
                match s.parse::<f64>() {
                    Ok(v) if v.is_finite() => {}
                    _ => return false,
                }
            }
        }
        true
    }
}

///
/// # Safety
/// JSON string pointers must be valid NUL-terminated C strings
/// (or null where documented).
#[no_mangle]
pub unsafe extern "C" fn mimi_from_json(
    json_str: *const std::ffi::c_char,
) -> *mut std::ffi::c_void {
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

///
/// # Safety
/// JSON string pointers must be valid NUL-terminated C strings
/// (or null where documented).
#[no_mangle]
pub unsafe extern "C" fn mimi_is_valid_json(json_str: *const std::ffi::c_char) -> i64 {
    if json_str.is_null() {
        return 0;
    }
    // SAFETY: `json_str` was checked non-null above.
    let s = unsafe { cstr_to_string(json_str) };
    let mut parser = JsonParser::new(&s);
    parser.is_valid() as i64
}

/// Walk a top-level JSON object and extract the value for `key`.
///
/// audit-wave1 (fail-loud JSON accessors): distinguishes the three outcomes
/// the extern accessors need:
/// - `Ok(Some(val))` — well-formed object, key present, value parsed.
/// - `Ok(None)`      — input is a well-formed JSON value but the key is
///   absent (top-level value not an object, or key missing).
///   VM parity: `jv.get(key)` on a non-object is `None`.
/// - `Err(())`       — input is not well-formed JSON (parse failure), or a
///   null pointer was passed.
fn json_get_inner(
    json_str: *const std::ffi::c_char,
    key: *const std::ffi::c_char,
) -> Result<Option<String>, ()> {
    if json_str.is_null() || key.is_null() {
        return Err(());
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
    if pos >= bytes.len() {
        return Err(());
    }
    if bytes[pos] != b'{' {
        // Not an object. If it still parses as some JSON value, the key is
        // simply absent (VM: `get` on non-object → None). Unparseable input
        // is a genuine parse error.
        let mut probe = JsonParser::new(&json[pos..]);
        return if probe.parse_value().is_some() {
            Ok(None)
        } else {
            Err(())
        };
    }
    pos += 1;

    loop {
        while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') {
            pos += 1;
        }
        if pos >= bytes.len() {
            return Err(()); // unterminated object
        }
        if bytes[pos] == b'}' {
            return Ok(None); // well-formed object, key not present
        }

        // Parse key string
        if bytes[pos] != b'"' {
            return Err(());
        }
        pos += 1;
        let mut key_buf = String::new();
        let mut key_esc = false;
        loop {
            if pos >= bytes.len() {
                return Err(());
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
                        // audit-wave1: surrogate pairs combined, lone
                        // surrogates / malformed hex fail the parse.
                        let (ch, consumed) = json_decode_unicode_escape(bytes, pos + 1).ok_or(())?;
                        key_buf.push(ch);
                        pos += consumed;
                    }
                    _ => return Err(()),
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
            // RFC 8259: raw control characters are not allowed in strings.
            if c < 0x20 {
                return Err(());
            }
            key_buf.push(c as char);
            pos += 1;
        }

        while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') {
            pos += 1;
        }
        if pos >= bytes.len() || bytes[pos] != b':' {
            return Err(());
        }
        pos += 1;
        while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') {
            pos += 1;
        }

        if key_buf == k {
            // Extract the value at current position
            let val_start = pos;
            let mut parser = JsonParser::new(&json[val_start..]);
            return parser.parse_value().ok_or(()).map(Some);
        }

        // Skip value
        let val_start = pos;
        let mut dummy_parser = JsonParser::new(&json[val_start..]);
        if dummy_parser.parse_value().is_none() {
            return Err(());
        }
        pos = val_start + dummy_parser.pos;

        while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') {
            pos += 1;
        }
        if pos >= bytes.len() {
            return Err(());
        }
        if bytes[pos] == b',' {
            pos += 1;
        }
    }
}

/// audit-wave1 (fail-loud JSON accessors): abort helper. Builds a
/// NUL-terminated message of the form `{prefix}{detail}{suffix}` and aborts.
/// The temporary leaks is irrelevant — the process terminates.
fn json_accessor_abort(prefix: &str, detail: &str, suffix: &str) -> ! {
    let mut msg = String::with_capacity(prefix.len() + detail.len() + suffix.len() + 1);
    msg.push_str(prefix);
    msg.push_str(detail);
    msg.push_str(suffix);
    msg.push('\0');
    unsafe { mimi_runtime_abort(msg.as_ptr() as *const std::ffi::c_char) }
}

/// True if the document parses as SOME JSON value (used by accessors to tell
/// "well-formed but wrong shape" apart from "malformed input").
fn json_parses_as_value(json: &str) -> bool {
    let mut probe = JsonParser::new(json);
    probe.parse_value().is_some()
}

/// Read `key` for error messages (defensive: null → empty label).
fn json_key_label(key: *const std::ffi::c_char) -> String {
    if key.is_null() {
        return String::new();
    }
    // SAFETY: callers pass codegen-owned NUL-terminated strings; the
    // non-null case here is only reached from accessors that already
    // validated non-null before deciding to build a message.
    unsafe { cstr_to_string(key) }
}

/// audit-wave1 (fail-loud, audit §10): aborts instead of returning NULL.
/// VM-matching messages (src/interp/bytecode/builtins/misc.rs):
/// malformed JSON, missing key. Codegen keeps its NULL guards as defense.
///
/// # Safety
/// JSON string pointers must be valid NUL-terminated C strings
/// (or null where documented).
#[no_mangle]
pub unsafe extern "C" fn json_get_string(
    json_str: *const std::ffi::c_char,
    key: *const std::ffi::c_char,
) -> *mut std::ffi::c_char {
    match json_get_inner(json_str, key) {
        Ok(Some(val)) => alloc_c_string(&val),
        Ok(None) => {
            let k = json_key_label(key);
            json_accessor_abort("json_get_string: key '", &k, "' not found")
        }
        Err(()) => json_accessor_abort("json_get_string parse error: ", "invalid JSON", ""),
    }
}

/// CRITICAL #18 fix: Check if a key exists in a JSON object.
/// Returns 1 if the key exists, 0 if not. This avoids the ambiguity of
/// json_get_string returning "" for both missing keys and empty-string values.
///
/// audit-wave1: a MISSING key still returns 0 (that is the function's
/// purpose, VM parity); malformed JSON now aborts instead of masquerading
/// as "key absent".
///
/// # Safety
/// JSON string pointers must be valid NUL-terminated C strings
/// (or null where documented).
#[no_mangle]
pub unsafe extern "C" fn json_has_key(
    json_str: *const std::ffi::c_char,
    key: *const std::ffi::c_char,
) -> i64 {
    // json_get_inner returns Ok(None) when key is missing, Ok(Some) when key
    // exists (regardless of value content). This correctly distinguishes
    // {"x": ""} (key exists) from {} (key missing). Err = malformed JSON.
    match json_get_inner(json_str, key) {
        Ok(Some(_)) => 1,
        Ok(None) => 0,
        Err(()) => json_accessor_abort("json_has_key parse error: ", "invalid JSON", ""),
    }
}

/// audit-wave1 (fail-loud, audit §10): aborts with VM-matching messages on
/// malformed JSON / missing key / wrong type instead of returning 0.
///
/// # Safety
/// JSON string pointers must be valid NUL-terminated C strings
/// (or null where documented).
#[no_mangle]
pub unsafe extern "C" fn json_get_int(
    json_str: *const std::ffi::c_char,
    key: *const std::ffi::c_char,
) -> i64 {
    let inner = json_get_inner(json_str, key);
    let val = match inner {
        Err(()) => json_accessor_abort("json_get_int parse error: ", "invalid JSON", ""),
        Ok(None) => {
            let k_label = json_key_label(key);
            json_accessor_abort("json_get_int: key '", &k_label, "' not found")
        }
        Ok(Some(val)) => val,
    };
    // The hand parser flattens values to strings; numeric-ness is re-derived
    // here (see the JSON parser section above for the audit-closure note).
    if let Ok(v) = val.parse::<i64>() {
        return v;
    }
    let k_label = json_key_label(key);
    if val.parse::<f64>().is_ok() {
        // Number but not integral (e.g. 1.5) — VM: "is not an integer".
        json_accessor_abort(
            "json_get_int: value for key '",
            &k_label,
            "' is not an integer",
        )
    } else {
        // String/bool/null/object/array — VM: "is not a number".
        json_accessor_abort("json_get_int: key '", &k_label, "' is not a number")
    }
}

/// Internal sentinel variant: 0 on null/non-array/malformed input.
/// Used by runtime-internal consumers (mimi_set_from_json_*) whose
/// historical behavior is "empty set on unusable input"; the PUBLIC extern
/// below fails loud instead.
fn json_array_length_try(json_str: *const std::ffi::c_char) -> i64 {
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

/// audit-wave1 (fail-loud, audit §10): aborts with VM-matching messages on
/// malformed JSON / non-array input instead of silently returning 0.
///
/// # Safety
/// JSON string pointers must be valid NUL-terminated C strings
/// (or null where documented).
#[no_mangle]
pub unsafe extern "C" fn json_array_length(json_str: *const std::ffi::c_char) -> i64 {
    if json_str.is_null() {
        json_accessor_abort("json_array_length parse error: ", "invalid JSON", "");
    }
    // SAFETY: `json_str` was checked non-null above.
    let json = unsafe { cstr_to_string(json_str) };
    let first = json
        .bytes()
        .enumerate()
        .find(|(_, b)| !matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
        .map(|(i, _)| i);
    match first {
        None => json_accessor_abort("json_array_length parse error: ", "invalid JSON", ""),
        Some(i) if json.as_bytes()[i] != b'[' => {
            // VM parity: a well-formed non-array value → "not an array";
            // anything unparseable is a parse error.
            if json_parses_as_value(&json[i..]) {
                json_accessor_abort("json_array_length: ", "value is not an array", "")
            } else {
                json_accessor_abort("json_array_length parse error: ", "invalid JSON", "")
            }
        }
        _ => {}
    }
    json_array_length_try(json_str)
}

/// Internal sentinel variant: null on malformed JSON / non-array / OOB index.
/// Used by runtime-internal consumers (mimi_set_from_json_*) whose historical
/// behavior is "skip unusable elements"; the PUBLIC extern below fails loud.
fn json_get_element_try(
    json_str: *const std::ffi::c_char,
    index: i64,
) -> Option<*mut std::ffi::c_char> {
    if json_str.is_null() {
        return None;
    }
    // SAFETY: `json_str` was checked non-null above.
    let json = unsafe { cstr_to_string(json_str) };
    let bytes = json.as_bytes();
    let mut pos = 0;

    while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') {
        pos += 1;
    }
    if pos >= bytes.len() || bytes[pos] != b'[' {
        return None;
    }
    pos += 1;

    let mut idx: i64 = 0;
    loop {
        if pos >= bytes.len() || bytes[pos] == b']' {
            return None;
        }
        while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') {
            pos += 1;
        }

        if idx == index {
            let val_start = pos;
            let mut parser = JsonParser::new(&json[val_start..]);
            return parser.parse_value().map(|val| alloc_c_string(&val));
        }

        let val_start = pos;
        let mut dummy_parser = JsonParser::new(&json[val_start..]);
        if dummy_parser.parse_value().is_none() {
            return None;
        }
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
        idx += 1;
    }
}

/// audit-wave1 (fail-loud, audit §10): aborts with VM-matching messages on
/// malformed JSON / out-of-bounds index instead of returning NULL.
///
/// # Safety
/// JSON string pointers must be valid NUL-terminated C strings
/// (or null where documented).
#[no_mangle]
pub unsafe extern "C" fn json_get_element(
    json_str: *const std::ffi::c_char,
    index: i64,
) -> *mut std::ffi::c_char {
    if json_str.is_null() {
        json_accessor_abort("json_get_element parse error: ", "invalid JSON", "");
    }
    // SAFETY: `json_str` was checked non-null above.
    let json = unsafe { cstr_to_string(json_str) };
    let first = json
        .bytes()
        .enumerate()
        .find(|(_, b)| !matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
        .map(|(i, _)| i);
    match first {
        None => json_accessor_abort("json_get_element parse error: ", "invalid JSON", ""),
        Some(i) if json.as_bytes()[i] != b'[' => {
            // VM parity: `.get(idx)` on a well-formed non-array is None →
            // "index out of bounds"; unparseable input is a parse error.
            if json_parses_as_value(&json[i..]) {
                let idx_s = index.to_string();
                json_accessor_abort("json_get_element: index ", &idx_s, " out of bounds")
            } else {
                json_accessor_abort("json_get_element parse error: ", "invalid JSON", "")
            }
        }
        _ => {}
    }
    match json_get_element_try(json_str, index) {
        Some(ptr) => ptr,
        None => {
            let idx_s = index.to_string();
            json_accessor_abort("json_get_element: index ", &idx_s, " out of bounds")
        }
    }
}

// ─── from_json::<T> typed parsing helpers ────────────────────────

/// Serialize a MapHandle of integer ValueHandles to a JSON object string.
/// Keys are JSON-escaped; values are printed as decimal integers.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_i64(handle: MapHandle) -> *mut std::ffi::c_char {
    map_to_json_values(handle, MapJsonMode::Int)
}

/// Serialize a MapHandle of 0/1 bool ValueHandles as JSON true/false.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_bool(handle: MapHandle) -> *mut std::ffi::c_char {
    map_to_json_values(handle, MapJsonMode::Bool)
}

/// Serialize a MapHandle of f64-bit ValueHandles for println Display (compact).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_f64(handle: MapHandle) -> *mut std::ffi::c_char {
    map_to_json_values(handle, MapJsonMode::Float)
}

/// Serialize Map f64 for `to_json` (serde-compatible, whole floats as `2.0`).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_f64_serde(handle: MapHandle) -> *mut std::ffi::c_char {
    map_to_json_values(handle, MapJsonMode::FloatJson)
}

enum MapJsonMode {
    Int,
    Bool,
    Float,
    FloatJson,
    String,
}

unsafe fn map_to_json_values(handle: MapHandle, mode: MapJsonMode) -> *mut std::ffi::c_char {
    if handle == 0 {
        return alloc_c_string("{}");
    }
    // SAFETY: handle is a non-zero MapHandle from mimi_map_new / from_json.
    let map = map_from_handle(handle);
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
///
/// # Safety
/// JSON string pointers must be valid NUL-terminated C strings
/// (or null where documented).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_f64(json: *const std::ffi::c_char) -> MapHandle {
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
            map_from_handle(handle)
                .inner
                .insert(key, bits as ValueHandle);
        }
        count += 1;
    }
    handle
}

/// Build a MapHandle from a JSON object with string keys and string values.
/// Values are heap-cloned C strings (ValueHandles via mimi_str_clone).
///
/// # Safety
/// JSON string pointers must be valid NUL-terminated C strings
/// (or null where documented).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_string(json: *const std::ffi::c_char) -> MapHandle {
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
        let v_handle =
            unsafe { mimi_str_clone(val.as_ptr() as *const std::ffi::c_char, val.len() as i64) };
        // SAFETY: handle is a valid map from mimi_map_new.
        unsafe {
            map_from_handle(handle).inner.insert(key, v_handle);
        }
        count += 1;
    }
    handle
}

/// Serialize a MapHandle whose values are C-string ValueHandles to JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_string(handle: MapHandle) -> *mut std::ffi::c_char {
    if handle == 0 {
        return alloc_c_string("{}");
    }
    // SAFETY: handle is a non-zero MapHandle.
    let map = map_from_handle(handle);
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
pub unsafe extern "C" fn mimi_map_to_json_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        // §10-#31: probe alignment + mapping before reading the heap-packed
        // i64[n] struct (previously a bare from_raw_parts — segfault on
        // corrupt handles). vh == 0 is the legitimate empty-value sentinel.
        let fields: Vec<i64> = if vh == 0 {
            vec![0; n]
        } else {
            safe_read_product_fields(vh, n).unwrap_or_else(|| vec![0; n])
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
pub unsafe extern "C" fn mimi_map_to_json_list_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        // §10-#31: probe the 16-byte list header before dereferencing it.
        let list_base = vh as *const u8;
        if !pages_mapped(vh as usize, 16) {
            if !PRODUCT_HANDLE_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                eprintln!(
                    "[mimi runtime] list-product value handle {:#x} points at unmapped memory — serialized as []",
                    vh
                );
            }
            parts.push(String::from("[]"));
            continue;
        }
        // SAFETY: pages_mapped confirmed the 16-byte header is mapped.
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
            // SAFETY: the offset is within bounds of `data`'s allocated buffer
            let prod_h = unsafe { *data.offset(j) };
            // §10-#31: mincore-probed read (bare from_raw_parts segfaulted
            // on corrupt product handles).
            let fields: Vec<i64> = if prod_h == 0 {
                vec![0; n]
            } else {
                safe_read_product_fields(prod_h as ValueHandle, n).unwrap_or_else(|| vec![0; n])
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
                // SAFETY: the offset is within bounds of `data`'s allocated buffer
                let prod_h = unsafe { *data.offset(j) };
                // §10-#31: mincore-probed read.
                let fields: Vec<i64> = if prod_h == 0 {
                    vec![0; n]
                } else {
                    safe_read_product_fields(prod_h as ValueHandle, n).unwrap_or_else(|| vec![0; n])
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
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_list_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
            // SAFETY: `size` is a valid, non-negative allocation size
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
        // SAFETY: `size` is a valid, non-negative allocation size
        let list_ptr = unsafe { libc::malloc(list_size) as *mut u8 };
        if list_ptr.is_null() {
            continue;
        }
        let data_size = prod_handles.len() * std::mem::size_of::<i64>();
        let data_ptr = if data_size > 0 {
            // SAFETY: `size` is a valid, non-negative allocation size
            unsafe { libc::malloc(data_size) as *mut i64 }
        } else {
            std::ptr::null_mut()
        };
        if !data_ptr.is_null() && !prod_handles.is_empty() {
            // SAFETY: `data_ptr` was just allocated by `libc::malloc` with `data_size` bytes; `prod_handles` is a local `Vec`; source and destination are non-overlapping
            unsafe {
                std::ptr::copy_nonoverlapping(prod_handles.as_ptr(), data_ptr, prod_handles.len());
            }
        }
        // SAFETY: `list_ptr` was just allocated with 16 bytes by `libc::malloc` (non-null checked); writes at offsets 0 and 8 are within bounds
        unsafe {
            *(list_ptr as *mut i64) = prod_handles.len() as i64;
            *(list_ptr.add(8) as *mut *mut i64) = data_ptr;
        }
        let vh = list_ptr as ValueHandle;
        // SAFETY: `map_from_handle(handle)` returns a valid, properly aligned pointer; `key` is a valid `String`
        unsafe {
            let mut map_ptr = map_from_handle(handle);
            (*map_ptr).inner.insert(key, vh);
            // §10-#35: the list header + data array + element packs were all
            // malloc'd above — register so destroy() can reclaim them.
            (*map_ptr).owned.insert(vh, MapOwnedValueKind::ListOfPacks);
        }
        MAP_OWNED_VALUE_BALANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    handle
}

/// Serialize Map values that are SetHandles of product-tuples.
/// `display_style`: 0 = JSON set arrays, 1 = Display `Set{(…)}`.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_set_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        // SAFETY: `set_json` is a heap-allocated C string (or heap block) that was returned by a prior allocation; `mimi_free` is the matching deallocation (mimi_alloc/alloc_c_string path)
        let s = unsafe { cstr_to_string(set_json) };
        if !set_json.is_null() {
            // SAFETY: `mut` points to a valid, properly aligned value
            mimi_free(set_json as *mut std::ffi::c_void);
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Build Map from JSON object whose values are product-set arrays:
/// `"a":[[1,2],[3,4]]` → Map string → Set of product.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_set_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
        // SAFETY: `map_from_handle(handle)` returned a valid pointer; inserts a `SetHandle` that was just allocated
        unsafe {
            map_from_handle(handle)
                .inner
                .insert(key, set_h as ValueHandle);
        }
    }
    handle
}

/// Serialize Map values that are MapHandles of product-tuples.
/// `display_style`: 0 = JSON, 1 = Display with `(a, b)` products.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_map_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        // SAFETY: `inner_json` is a heap-allocated C string (or heap block) that was returned by a prior allocation; `mimi_free` is the matching deallocation (mimi_alloc/alloc_c_string path)
        let s = unsafe { cstr_to_string(inner_json) };
        if !inner_json.is_null() {
            // SAFETY: `mut` points to a valid, properly aligned value
            mimi_free(inner_json as *mut std::ffi::c_void);
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Build Map from JSON object whose values are nested Map product objects:
/// `"outer":{"a":[1,2]}`.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_map_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
        // SAFETY: `map_from_handle(handle)` returned a valid pointer; inserts a `MapHandle` that was just allocated
        unsafe {
            map_from_handle(handle)
                .inner
                .insert(key, inner_h as ValueHandle);
        }
    }
    handle
}

/// Build a MapHandle from a JSON object whose values are product-tuple arrays
/// of integers (e.g. `"a":[1,2]`). Each value is heap-packed as i64[arity]
/// matching `map_set` of product tuples / `mimi_map_to_json_product_i64`.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
        // SAFETY: `size` is a valid, non-negative allocation size
        let ptr = unsafe { libc::malloc(data_size) as *mut i64 };
        if ptr.is_null() {
            continue;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(fields.as_ptr(), ptr, n);
        }
        let vh = ptr as ValueHandle;
        // SAFETY: `map_from_handle(handle)` returned a valid pointer; `key` is a valid `String` and `vh` is a heap-packed product pointer
        unsafe {
            let mut map_ptr = map_from_handle(handle);
            (*map_ptr).inner.insert(key, vh);
            // §10-#35: register the malloc'd pack so destroy() reclaims it.
            (*map_ptr).owned.insert(vh, MapOwnedValueKind::Pack);
        }
        MAP_OWNED_VALUE_BALANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    handle
}

/// Map of Result of Map of product from JSON.
/// Pack: `{i64 disc, i64 map_handle_or_err}` disc 1=Ok map handle, 0=Err string ptr.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_result_map_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
        // SAFETY: `size` is a valid, non-negative allocation size
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            continue;
        }
        if bytes[i] != b'{' {
            // SAFETY: `pack` was just allocated by `libc::malloc` and is non-null (checked at line 4628); freeing before return prevents the leak
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
            // SAFETY: `pack` is non-null with 16 bytes allocated; offsets 0 and 8 are within bounds for writing two `i64` values
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
                // SAFETY: `c_obj` was returned by `alloc_c_string` and is non-null (checked by the enclosing `if`); freed to prevent memory leak
                mimi_free(c_obj as *mut _);
            }
            // SAFETY: `pack` is non-null with 16 bytes; writing `disc` and `inner_h` at offsets 0 and 8 is within bounds
            unsafe {
                *pack = 1;
                *pack.add(1) = inner_h as i64;
            }
        }
        // SAFETY: `map_from_handle(handle)` returned a valid pointer; `pack` is a valid heap-allocated result struct
        let vh = pack as ValueHandle;
        // SAFETY: `map_from_handle(handle)` returns a valid, properly aligned pointer; `key` is a valid `String`
        unsafe {
            let mut map_ptr = map_from_handle(handle);
            (*map_ptr).inner.insert(key, vh);
            // §10-#35: this pack was malloc'd by the builder — register so
            // destroy() can reclaim its base (inner object handles are
            // intentional bounded leaks, see mimi_map_destroy comment).
            (*map_ptr)
                .owned
                .insert(vh, MapOwnedValueKind::PackErrCString);
        }
        MAP_OWNED_VALUE_BALANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    handle
}

/// Map of Result of Map of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_result_map_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        // SAFETY: `base` points to a valid, properly aligned value
        if !pages_mapped(base as usize, 16) {
            parts.push(String::from("null"));
            continue;
        }
        let disc = unsafe { *base };
        if disc == 0 {
            // SAFETY: `err_ptr` is a valid null-terminated C string from a prior allocation
            let err_ptr = unsafe { *base.add(1) } as *const std::ffi::c_char;
            let err_s = if err_ptr.is_null() {
                String::new()
            } else {
                // SAFETY: `err_ptr` is a valid null-terminated C string returned by a Mimi allocation function
                unsafe { cstr_to_string(err_ptr) }
            };
            if display_style != 0 {
                parts.push(format!("Err({})", err_s));
            } else {
                parts.push(format!("{{\"Err\":[{}]}}", json_escape_string(&err_s)));
            }
        } else {
            // SAFETY: the offset is within bounds of `base`'s allocation; the handle value is valid for the target type
            let inner_h = unsafe { *base.add(1) } as MapHandle;
            let inner_json = mimi_map_to_json_product_i64(inner_h, arity, display_style);
            // SAFETY: `inner_json` is a valid null-terminated C string returned by a Mimi allocation function
            let s = unsafe { cstr_to_string(inner_json) };
            if !inner_json.is_null() {
                mimi_free(inner_json as *mut _);
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
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_option_result_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
        // SAFETY: `size` is a valid, non-negative allocation size
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            continue;
        }
        let mut res_h: i64 = 0;
        let mut transferred_owned_kind: Option<MapOwnedValueKind> = None;
        let is_none = if bytes[i] == b'n' && i + 4 <= bytes.len() && &bytes[i..i + 4] == b"null" {
            i += 4;
            true
        } else {
            false
        };
        if is_none {
            // SAFETY: `pack` was just allocated with 16 bytes by `libc::malloc` (non-null checked); writing two `i64` values at offsets 0 and 8 is within bounds
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
                // SAFETY: `pack` was just allocated by `libc::malloc` and is non-null; freeing prevents the leak when the input format is unexpected
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
                // SAFETY: `c_one` was returned by `alloc_c_string` and is non-null (checked by the enclosing `if`); freed to prevent memory leak
                mimi_free(c_one as *mut _);
            }
            // Extract the single value handle from tmp_map, transferring its
            // ownership record to the outer map before destroying the temporary.
            if tmp_map != 0 {
                // SAFETY: `map_from_handle` is non-null and points to a valid map instance.
                let mut m = map_from_handle(tmp_map);
                if let Some(&v) = m.inner.values().next() {
                    res_h = v as i64;
                    transferred_owned_kind = m.owned.remove(&v);
                }
                // Drop the entries from the temporary without freeing the
                // transferred value; mimi_map_destroy below is then a no-op
                // for that value.
                m.inner.clear();
            }
            if tmp_map != 0 {
                unsafe {
                    mimi_map_destroy(tmp_map);
                }
            }
            // SAFETY: `pack` is non-null with 16 bytes; writing the Some discriminant and result handle at offsets 0 and 8 is within bounds
            unsafe {
                *pack = 1;
                *pack.add(1) = res_h;
            }
            let _ = n;
        }
        // SAFETY: `map_from_handle(handle)` returned a valid pointer; `pack` is a valid heap-allocated Option-of-Result struct
        let vh = pack as ValueHandle;
        // SAFETY: `map_from_handle(handle)` returns a valid, properly aligned pointer; `key` is a valid `String`
        unsafe {
            let mut map_ptr = map_from_handle(handle);
            (*map_ptr).inner.insert(key, vh);
            // §10-#35: this pack was malloc'd by the builder — register so
            // destroy() can reclaim its base (inner object handles are
            // intentional bounded leaks, see mimi_map_destroy comment).
            (*map_ptr).owned.insert(vh, MapOwnedValueKind::Pack);
            // If the temporary JSON map owned the inner result product,
            // transfer that ownership record to the outer map so the value
            // stays alive and the temporary map is destroyed without leaks.
            if let Some(kind) = transferred_owned_kind {
                if res_h != 0 {
                    (*map_ptr).owned.insert(res_h as ValueHandle, kind);
                }
            }
        }
        MAP_OWNED_VALUE_BALANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    handle
}

/// Map of Option of Result of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_option_result_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        // SAFETY: `base` points to a valid, properly aligned value
        if !pages_mapped(base as usize, 16) {
            parts.push(String::from("\"None\""));
            continue;
        }
        let disc = unsafe { *base };
        if disc == 0 {
            if display_style != 0 {
                parts.push(String::from("None()"));
            } else {
                parts.push(String::from("\"None\""));
            }
        } else {
            // SAFETY: the offset is within bounds of `base`'s allocated buffer
            let res_h = unsafe { *base.add(1) };
            let tmp = mimi_map_new();
            if tmp != 0 {
                unsafe {
                    map_from_handle(tmp)
                        .inner
                        .insert("_".into(), res_h as ValueHandle);
                }
                let json_ptr = mimi_map_to_json_result_product_i64(tmp, arity, display_style);
                // SAFETY: `json_ptr` is a valid null-terminated C string returned by a Mimi allocation function
                let s = unsafe { cstr_to_string(json_ptr) };
                unsafe {
                    mimi_map_destroy(tmp);
                }
                if !json_ptr.is_null() {
                    mimi_free(json_ptr as *mut _);
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
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_from_json_option_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> *mut MimiList {
    // audit-wave1 CRITICAL (allocator mismatch): struct allocated via Box and
    // freed via Box::from_raw in mimi_list_free; element_kind set explicitly
    // (elements are malloc'd `{disc, i64[n]}` option-product packs → Record).
    let empty = || Box::into_raw(Box::new(MimiList::new_with_kind(ListElementKind::Record)));
    if json.is_null() || arity <= 0 || arity > 16 {
        return empty();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
        // SAFETY: `size` is a valid, non-negative allocation size
        let ptr = unsafe { libc::malloc(pack_size) as *mut i64 };
        if ptr.is_null() {
            break;
        }
        if bytes[i] == b'n' && i + 4 <= bytes.len() && &bytes[i..i + 4] == b"null" {
            i += 4;
            // SAFETY: `ptr` was allocated with `pack_size = 8 + n*8` bytes; writing the None discriminant and zeroing `n` fields is within bounds
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
            // SAFETY: `ptr` was allocated with `pack_size` bytes for `1+n` i64 values; `fields` is a local `Vec` of length `n`; source and destination are non-overlapping
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
                // SAFETY: `ptr` was allocated with space for `1+n` i64 values; writing the None discriminant and zeroing fields is within bounds
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
                // SAFETY: `ptr` was allocated with space for `1+n` i64 values; `fields` is a local `Vec` of length `n`; source and destination are non-overlapping
                unsafe {
                    *ptr = 1;
                    std::ptr::copy_nonoverlapping(fields.as_ptr(), ptr.add(1), n);
                }
            }
        } else {
            // SAFETY: `ptr` was just allocated by `libc::malloc` and is non-null; freeing prevents the leak when the JSON format is unrecognized
            unsafe {
                libc::free(ptr as *mut _);
            }
            break;
        }
        handles.push(ptr as i64);
    }
    let data_size = handles.len() * std::mem::size_of::<i64>();
    let data = if data_size > 0 {
        // SAFETY: `data_size` is positive; result is checked for null below.
        unsafe { libc::malloc(data_size) as *mut i64 }
    } else {
        std::ptr::null_mut()
    };
    if !data.is_null() && !handles.is_empty() {
        // SAFETY: `data` was just allocated with `data_size` bytes by `libc::malloc` (non-null checked); `handles` is a local `Vec`; source and destination are non-overlapping
        unsafe {
            std::ptr::copy_nonoverlapping(handles.as_ptr(), data, handles.len());
        }
    }
    // audit-wave1 CRITICAL: Box-allocated struct (mimi_list_free frees via
    // Box::from_raw) + explicit element_kind (malloc'd option-product packs).
    Box::into_raw(Box::new(MimiList::with_data(
        data as *mut *mut std::ffi::c_char,
        handles.len() as i64,
        true,
        ListElementKind::Record,
    )))
}

/// List of Option of product Display/JSON.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_option_product_to_json(
    list: *const MimiList,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if list.is_null() || arity <= 0 || arity > 16 {
        return alloc_c_string("[]");
    }
    // SAFETY: `list` is non-null and points to a valid `MimiList`
    let lst = unsafe { &*list.cast::<MimiListAbiPrefix>() };
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
        // SAFETY: `lst.data` points to a valid, properly aligned value
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
        // SAFETY: `base` points to a valid, properly aligned value.
        if !pages_mapped(base as usize, 8 * (n + 1)) {
            parts.push(String::from("\"None\""));
            continue;
        }
        let disc = unsafe { *base };
        if disc == 0 {
            if display_style != 0 {
                parts.push(String::from("None()"));
            } else {
                parts.push(String::from("\"None\""));
            }
        } else {
            // SAFETY: `base.add(1)` points to at least `n` valid, properly aligned `i64` values; the pointer was derived from a heap-packed product allocation
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
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_from_json_option_set_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> *mut MimiList {
    // audit-wave1 CRITICAL (allocator mismatch): Box allocation + explicit
    // element_kind (elements are malloc'd `{opt_disc, set_handle}` packs).
    let empty = || Box::into_raw(Box::new(MimiList::new_with_kind(ListElementKind::Record)));
    if json.is_null() || arity <= 0 || arity > 16 {
        return empty();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
        // SAFETY: `size` is a valid, non-negative allocation size
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            break;
        }
        if bytes[i] == b'n' && i + 4 <= bytes.len() && &bytes[i..i + 4] == b"null" {
            i += 4;
            // SAFETY: `pack` was just allocated with 16 bytes by `libc::malloc` (non-null checked); writing the None discriminant and zero handle is within bounds
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
                // SAFETY: `pack` was just allocated by `libc::malloc` and is non-null (checked at line 5377); freeing prevents the leak on unrecognized JSON
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
                // SAFETY: `c_arr` was allocated by `alloc_c_string` (which uses `libc::malloc`) and the null check above ensures it is a valid pointer for deallocation
                mimi_free(c_arr as *mut _);
            }
            // SAFETY: `pack` was allocated by `libc::malloc(16)` and checked as non-null, so `pack` and `pack.add(1)` point to valid, aligned memory
            unsafe {
                *pack = 1;
                *pack.add(1) = set_h as i64;
            }
        }
        handles.push(pack as i64);
    }
    let data_size = handles.len() * 8;
    let data = if data_size == 0 {
        std::ptr::null_mut()
    } else {
        // SAFETY: `data_size` is positive; result is checked for null below.
        unsafe { libc::malloc(data_size) as *mut i64 }
    };
    if data_size > 0 && data.is_null() {
        return empty();
    }
    if !data.is_null() {
        // SAFETY: `handles.as_ptr()` points to `handles.len()` initialized elements; `data` is non-null and was allocated with sufficient capacity for `handles.len()` i64 values; source and destination do not overlap
        unsafe {
            std::ptr::copy_nonoverlapping(handles.as_ptr(), data, handles.len());
        }
    }
    // audit-wave1 CRITICAL: Box-allocated struct + explicit element_kind
    // (malloc'd `{opt_disc, set_handle}` packs → Record).
    Box::into_raw(Box::new(MimiList::with_data(
        data as *mut *mut std::ffi::c_char,
        handles.len() as i64,
        true,
        ListElementKind::Record,
    )))
}

/// List of Option of Set of product Display/JSON.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_option_set_product_to_json(
    list: *const MimiList,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if list.is_null() || arity <= 0 || arity > 16 {
        return alloc_c_string("[]");
    }
    // SAFETY: `list` is non-null and points to a valid `MimiList`
    let lst = unsafe { &*list.cast::<MimiListAbiPrefix>() };
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
        // SAFETY: `lst.data` points to a valid, properly aligned value
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
        // SAFETY: `base` points to a valid, properly aligned value.
        if !pages_mapped(base as usize, 16) {
            parts.push(String::from("\"None\""));
            continue;
        }
        let disc = unsafe { *base };
        if disc == 0 {
            if display_style != 0 {
                parts.push(String::from("None()"));
            } else {
                parts.push(String::from("\"None\""));
            }
        } else {
            // SAFETY: the offset is within bounds of `base`'s allocation; the handle value is valid for the target type
            let set_h = unsafe { *base.add(1) } as SetHandle;
            let set_json = mimi_set_to_json_product_i64(set_h, arity, display_style);
            // SAFETY: `set_json` is a valid null-terminated C string returned by a Mimi allocation function
            let s = unsafe { cstr_to_string(set_json) };
            if !set_json.is_null() {
                mimi_free(set_json as *mut _);
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
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_list_option_set_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
            // SAFETY: `c_arr` was allocated by `alloc_c_string` (which uses `libc::malloc`) and the null check above ensures it is a valid pointer for deallocation
            mimi_free(c_arr as *mut _);
        }
        // SAFETY: `map_from_handle(handle)` returns a valid non-null pointer to a `MimiMap` instance; the handle was previously created by `mimi_map_new()` and is still alive
        let vh = list_ptr as ValueHandle;
        // SAFETY: `map_from_handle(handle)` returns a valid, properly aligned pointer; `key` is a valid `String`
        unsafe {
            let mut map_ptr = map_from_handle(handle);
            (*map_ptr).inner.insert(key, vh);
            // §10-#35: this list_ptr was malloc'd by the builder — register so
            // destroy() can reclaim its base (inner object handles are
            // intentional bounded leaks, see mimi_map_destroy comment).
            (*map_ptr).owned.insert(vh, MapOwnedValueKind::ListObject);
        }
        MAP_OWNED_VALUE_BALANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    handle
}

/// Map of List of Option of Set of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_list_option_set_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        let json_ptr =
            unsafe { mimi_list_option_set_product_to_json(list_ptr, arity, display_style) };
        // SAFETY: `json_ptr` is a valid null-terminated C string returned by a Mimi allocation function
        let s = unsafe { cstr_to_string(json_ptr) };
        if !json_ptr.is_null() {
            mimi_free(json_ptr as *mut _);
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of List of Option of product from JSON.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_list_option_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
            // SAFETY: `c_arr` was allocated by `alloc_c_string` (which uses `libc::malloc`) and the null check above ensures it is a valid pointer for deallocation
            mimi_free(c_arr as *mut _);
        }
        // SAFETY: `map_from_handle(handle)` returns a valid non-null pointer to a `MimiMap` instance; the handle was previously created by `mimi_map_new()` and is still alive
        let vh = list_ptr as ValueHandle;
        // SAFETY: `map_from_handle(handle)` returns a valid, properly aligned pointer; `key` is a valid `String`
        unsafe {
            let mut map_ptr = map_from_handle(handle);
            (*map_ptr).inner.insert(key, vh);
            // §10-#35: this list_ptr was malloc'd by the builder — register so
            // destroy() can reclaim its base (inner object handles are
            // intentional bounded leaks, see mimi_map_destroy comment).
            (*map_ptr).owned.insert(vh, MapOwnedValueKind::ListObject);
        }
        MAP_OWNED_VALUE_BALANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    handle
}

/// Map of List of Option of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_list_option_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        let json_ptr = unsafe { mimi_list_option_product_to_json(list_ptr, arity, display_style) };
        // SAFETY: `json_ptr` is a valid null-terminated C string returned by a Mimi allocation function
        let s = unsafe { cstr_to_string(json_ptr) };
        if !json_ptr.is_null() {
            mimi_free(json_ptr as *mut _);
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Set of Option of Result of product from JSON.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_set_from_json_option_result_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> SetHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_set_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
            // SAFETY: `size` is a valid, non-negative allocation size
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
            // SAFETY: `c_wrap` was allocated by `alloc_c_string` (which uses `libc::malloc`) and the null check above ensures it is a valid pointer for deallocation
            mimi_free(c_wrap as *mut _);
        }
        if tmp != 0 {
            // SAFETY: `map_from_handle` is non-null and points to a valid map instance
            let m = map_from_handle(tmp);
            if let Some(v) = m.inner.values().next() {
                mimi_set_insert(handle, *v as SetValueHandle);
            }
        }
    }
    handle
}

/// Set of Option of Result of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_set_to_json_option_result_product_i64(
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
    // SAFETY: `set_from_handle` is non-null and points to a valid `MimiSet`
    let set = set_from_handle(handle);
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
                    // SAFETY: `base` points to a valid, properly aligned value
                    let opt_disc = unsafe { *base };
                    if opt_disc == 0 {
                        (0i64, 0i64, String::new(), vec![0; n])
                    } else {
                        // SAFETY: the offset is within bounds of `base`'s allocated buffer
                        let res_h = unsafe { *base.add(1) };
                        if res_h == 0 {
                            (1i64, 0i64, String::new(), vec![0; n])
                        } else {
                            let rp = res_h as *const i64;
                            // SAFETY: `rp` points to a valid, properly aligned value
                            let res_disc = unsafe { *rp };
                            if res_disc == 0 {
                                // SAFETY: `err_ptr` is a valid null-terminated C string from a prior allocation
                                let err_ptr = unsafe { *rp.add(1) } as *const std::ffi::c_char;
                                let err_s = if err_ptr.is_null() {
                                    String::new()
                                } else {
                                    // SAFETY: the pointer is non-null and points to at least `n` initialized elements
                                    unsafe { cstr_to_string(err_ptr) }
                                };
                                (1i64, 0i64, err_s, vec![0; n])
                            } else {
                                let fields =
// SAFETY: `rp.add(1)` points to a valid allocation of at least `n + 1` i64 elements where `n = arity as usize` is in [1, 16]; the data at the pointer is properly initialized
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
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_set_from_json_result_option_product_i64(
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
    // SAFETY: `list_ptr` is non-null, properly aligned, and points to a valid value of the expected type
    let lst = unsafe { &*list_ptr.cast::<MimiListAbiPrefix>() };
    if !lst.data.is_null() && lst.len > 0 {
        for i in 0..lst.len as isize {
            // SAFETY: `lst.data` points to a valid, properly aligned value
            let h = unsafe { *(lst.data as *const i64).offset(i) };
            // Store opaque product handle as set value (same as set result product).
            mimi_set_insert(handle, h as SetValueHandle);
        }
    }
    handle
}

/// Set of Result of Option of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_set_to_json_result_option_product_i64(
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
    // SAFETY: `set_from_handle` is non-null and points to a valid `MimiSet`
    let set = set_from_handle(handle);
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
                    // SAFETY: `ptr` points to a valid, properly aligned value
                    let disc = unsafe { *ptr };
                    if disc == 0 {
                        // SAFETY: `err_ptr` is a valid null-terminated C string from a prior allocation
                        let err_ptr = unsafe { *ptr.add(1) } as *const std::ffi::c_char;
                        let err_s = if err_ptr.is_null() {
                            String::new()
                        } else {
                            // SAFETY: the pointer arithmetic is within bounds of the allocated buffer
                            unsafe { cstr_to_string(err_ptr) }
                        };
                        (0i64, err_s, 0i64, vec![0; n])
                    } else {
                        // SAFETY: the offset is within bounds of `ptr`'s allocated buffer
                        let opt_h = unsafe { *ptr.add(1) } as *const i64;
                        if opt_h.is_null() {
                            (1i64, String::new(), 0i64, vec![0; n])
                        } else {
                            // SAFETY: `opt_h` points to a valid, properly aligned value
                            let opt_disc = unsafe { *opt_h };
                            if opt_disc == 0 {
                                (1i64, String::new(), 0i64, vec![0; n])
                            } else {
                                let fields =
// SAFETY: `opt_h.add(1)` points to a valid allocation of at least `n + 1` i64 elements where `n = arity as usize` is in [1, 16]; the data at the pointer is properly initialized
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
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_set_from_json_list_map_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> SetHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_set_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
            // SAFETY: `c_arr` was allocated by `alloc_c_string` (which uses `libc::malloc`) and the null check above ensures it is a valid pointer for deallocation
            mimi_free(c_arr as *mut _);
        }
        mimi_set_insert(handle, list_ptr as SetValueHandle);
    }
    handle
}

/// Set of List of Map of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_set_to_json_list_map_product_i64(
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
    // SAFETY: `set_from_handle` is non-null and points to a valid `MimiSet`
    let set = set_from_handle(handle);
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
            let jp = unsafe { mimi_list_map_product_to_json(list_ptr, arity, display_style) };
            // SAFETY: `jp` is a valid null-terminated C string returned by a Mimi allocation function
            let s = unsafe { cstr_to_string(jp) };
            if !jp.is_null() {
                mimi_free(jp as *mut _);
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
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_set_from_json_result_list_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> SetHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_set_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
        // SAFETY: `size` is a valid, non-negative allocation size
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
            // SAFETY: `pack` was allocated by `libc::malloc(16)` at line 6402 and confirmed non-null at line 6403; `pack` and `pack.add(1)` point to valid, aligned memory
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
                // SAFETY: `c_wrap` was allocated by `alloc_c_string` (which uses `libc::malloc`) and the null check above ensures it is a valid pointer for deallocation
                mimi_free(c_wrap as *mut _);
            }
            let mut list_h: i64 = 0;
            if tmp != 0 {
                // SAFETY: `map_from_handle` is non-null and points to a valid map instance
                let m = map_from_handle(tmp);
                if let Some(v) = m.inner.values().next() {
                    list_h = *v as i64;
                }
            }
            // SAFETY: `pack` was allocated by `libc::malloc(16)` at line 6402 and confirmed non-null at line 6403; `pack` and `pack.add(1)` point to valid, aligned memory
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
pub unsafe extern "C" fn mimi_set_to_json_result_list_product_i64(
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
    // SAFETY: `set_from_handle` is non-null and points to a valid `MimiSet`
    let set = set_from_handle(handle);
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
            // SAFETY: `pack` points to a valid, properly aligned value
            let disc = unsafe { *pack };
            if disc == 0 {
                // SAFETY: `err_ptr` is a valid null-terminated C string from a prior allocation
                let err_ptr = unsafe { *pack.add(1) } as *const std::ffi::c_char;
                let err_s = if err_ptr.is_null() {
                    String::new()
                } else {
                    // SAFETY: `err_ptr` is a valid null-terminated C string returned by a Mimi allocation function
                    unsafe { cstr_to_string(err_ptr) }
                };
                let s = if display_style != 0 {
                    format!("Err({})", err_s)
                } else {
                    format!("{{\"Err\":[{}]}}", json_escape_string(&err_s))
                };
                (0, s)
            } else {
                // SAFETY: the offset is within bounds of `pack`'s allocated buffer
                let list_ptr = unsafe { *pack.add(1) } as *const MimiList;
                // Display list-of-product via one-entry map helper.
                let tmp = mimi_map_new();
                if tmp != 0 {
                    unsafe {
                        map_from_handle(tmp)
                            .inner
                            .insert(String::from("_"), list_ptr as ValueHandle);
                    }
                }
                let jp = mimi_map_to_json_list_product_i64(tmp, arity, display_style);
                // SAFETY: `jp` is a valid null-terminated C string returned by a Mimi allocation function
                let map_s = unsafe { cstr_to_string(jp) };
                unsafe {
                    mimi_map_destroy(tmp);
                }
                if !jp.is_null() {
                    mimi_free(jp as *mut _);
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
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_list_map_list_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
            // SAFETY: `c_arr` was allocated by `alloc_c_string` (which uses `libc::malloc`) and the null check above ensures it is a valid pointer for deallocation
            mimi_free(c_arr as *mut _);
        }
        // SAFETY: `map_from_handle(handle)` returns a valid non-null pointer to a `MimiMap` instance; the handle was previously created by `mimi_map_new()` and is still alive
        let vh = list_ptr as ValueHandle;
        // SAFETY: `map_from_handle(handle)` returns a valid, properly aligned pointer; `key` is a valid `String`
        unsafe {
            let mut map_ptr = map_from_handle(handle);
            (*map_ptr).inner.insert(key, vh);
            // §10-#35: this list_ptr was malloc'd by the builder — register so
            // destroy() can reclaim its base (inner object handles are
            // intentional bounded leaks, see mimi_map_destroy comment).
            (*map_ptr).owned.insert(vh, MapOwnedValueKind::ListObject);
        }
        MAP_OWNED_VALUE_BALANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    handle
}

/// List of Map of List of product from JSON.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_from_json_map_list_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> *mut MimiList {
    // audit-wave1 CRITICAL (allocator mismatch): Box allocation + explicit
    // element_kind (elements are MapHandles → Map, owned by the registry).
    let empty = || Box::into_raw(Box::new(MimiList::new_with_kind(ListElementKind::Map)));
    if json.is_null() || arity <= 0 || arity > 16 {
        return empty();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
            // SAFETY: `c_obj` was allocated by `alloc_c_string` (which uses `libc::malloc`) and the null check above ensures it is a valid pointer for deallocation
            mimi_free(c_obj as *mut _);
        }
        handles.push(mh as i64);
    }
    let data_size = handles.len() * 8;
    let data = if data_size == 0 {
        std::ptr::null_mut()
    } else {
        // SAFETY: `data_size` is positive; result is checked for null below.
        unsafe { libc::malloc(data_size) as *mut i64 }
    };
    if data_size > 0 && data.is_null() {
        return empty();
    }
    if !data.is_null() {
        // SAFETY: `handles.as_ptr()` points to `handles.len()` initialized elements; `data` is non-null and was allocated with sufficient capacity for `handles.len()` i64 values; source and destination do not overlap
        unsafe {
            std::ptr::copy_nonoverlapping(handles.as_ptr(), data, handles.len());
        }
    }
    // audit-wave1 CRITICAL: Box-allocated struct + explicit element_kind
    // (elements are MapHandles).
    Box::into_raw(Box::new(MimiList::with_data(
        data as *mut *mut std::ffi::c_char,
        handles.len() as i64,
        true,
        ListElementKind::Map,
    )))
}

/// List of Map of List of product Display/JSON.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_map_list_product_to_json(
    list: *const MimiList,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if list.is_null() || arity <= 0 || arity > 16 {
        return alloc_c_string("[]");
    }
    // SAFETY: `list` is non-null and points to a valid `MimiList`
    let lst = unsafe { &*list.cast::<MimiListAbiPrefix>() };
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
        // SAFETY: `lst.data` points to a valid, properly aligned value
        let h = unsafe { *(lst.data as *const i64).offset(i) };
        let map_json = mimi_map_to_json_list_product_i64(h as MapHandle, arity, display_style);
        // SAFETY: `map_json` is a valid null-terminated C string returned by a Mimi allocation function
        let s = unsafe { cstr_to_string(map_json) };
        if !map_json.is_null() {
            mimi_free(map_json as *mut _);
        }
        parts.push(s);
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

/// Map of List of Map of List of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_list_map_list_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        let json_ptr =
            unsafe { mimi_list_map_list_product_to_json(list_ptr, arity, display_style) };
        // SAFETY: `json_ptr` is a valid null-terminated C string returned by a Mimi allocation function
        let s = unsafe { cstr_to_string(json_ptr) };
        if !json_ptr.is_null() {
            mimi_free(json_ptr as *mut _);
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of Option of Map of List of product from JSON.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_option_map_list_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
        // SAFETY: `size` is a valid, non-negative allocation size
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            continue;
        }
        if bytes[i] == b'n' && i + 4 <= bytes.len() && &bytes[i..i + 4] == b"null" {
            i += 4;
            // SAFETY: `pack` was allocated by `libc::malloc(16)` at line 6951 and confirmed non-null at line 6952; `pack` and `pack.add(1)` point to valid, aligned memory
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
                // SAFETY: `c_obj` was allocated by `alloc_c_string` (which uses `libc::malloc`) and the null check above ensures it is a valid pointer for deallocation
                mimi_free(c_obj as *mut _);
            }
            // SAFETY: `pack` was allocated by `libc::malloc(16)` at line 6951 and confirmed non-null at line 6952; `pack` and `pack.add(1)` point to valid, aligned memory
            unsafe {
                *pack = 1;
                *pack.add(1) = mh as i64;
            }
        } else {
            // SAFETY: `pack` was allocated by `libc::malloc(16)` at line 6951 and confirmed non-null at line 6952; null check passed before reaching this branch; `pack` is a valid `libc::malloc` allocation
            unsafe {
                libc::free(pack as *mut _);
            }
            break;
        }
        // SAFETY: `map_from_handle(handle)` returns a valid non-null pointer to a `MimiMap` instance; the handle was previously created by `mimi_map_new()` and is still alive
        let vh = pack as ValueHandle;
        // SAFETY: `map_from_handle(handle)` returns a valid, properly aligned pointer; `key` is a valid `String`
        unsafe {
            let mut map_ptr = map_from_handle(handle);
            (*map_ptr).inner.insert(key, vh);
            // §10-#35: this pack was malloc'd by the builder — register so
            // destroy() can reclaim its base (inner object handles are
            // intentional bounded leaks, see mimi_map_destroy comment).
            (*map_ptr).owned.insert(vh, MapOwnedValueKind::Pack);
        }
        MAP_OWNED_VALUE_BALANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    handle
}

/// Map of Option of Map of List of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_option_map_list_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        // SAFETY: `base` points to a valid, properly aligned value.
        if !pages_mapped(base as usize, 16) {
            parts.push(String::from("null"));
            continue;
        }
        let disc = unsafe { *base };
        if disc == 0 {
            if display_style != 0 {
                parts.push(String::from("None()"));
            } else {
                parts.push(String::from("\"None\""));
            }
        } else {
            // SAFETY: the offset is within bounds of `base`'s allocation; the handle value is valid for the target type
            let mh = unsafe { *base.add(1) } as MapHandle;
            let map_json = mimi_map_to_json_list_product_i64(mh, arity, display_style);
            // SAFETY: `map_json` is a valid null-terminated C string returned by a Mimi allocation function
            let s = unsafe { cstr_to_string(map_json) };
            if !map_json.is_null() {
                mimi_free(map_json as *mut _);
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

///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_set_from_json_result_map_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> SetHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_set_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
        // SAFETY: `size` is a valid, non-negative allocation size
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
            // SAFETY: `pack` was allocated by `libc::malloc(16)` at line 7139 and confirmed non-null at line 7140; `pack` and `pack.add(1)` point to valid, aligned memory
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
                // SAFETY: `c_map` was allocated by `alloc_c_string` (which uses `libc::malloc`) and the null check above ensures it is a valid pointer for deallocation
                mimi_free(c_map as *mut _);
            }
            // SAFETY: `pack` was allocated by `libc::malloc(16)` at line 7139 and confirmed non-null at line 7140; `pack` and `pack.add(1)` point to valid, aligned memory
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
pub unsafe extern "C" fn mimi_set_to_json_result_map_product_i64(
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
    // SAFETY: `set_from_handle` is non-null and points to a valid `MimiSet`
    let set = set_from_handle(handle);
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
            // SAFETY: `pack` points to a valid, properly aligned value
            let disc = unsafe { *pack };
            if disc == 0 {
                // SAFETY: `err_ptr` is a valid null-terminated C string from a prior allocation
                let err_ptr = unsafe { *pack.add(1) } as *const std::ffi::c_char;
                let err_s = if err_ptr.is_null() {
                    String::new()
                } else {
                    // SAFETY: `err_ptr` is a valid null-terminated C string returned by a Mimi allocation function
                    unsafe { cstr_to_string(err_ptr) }
                };
                let s = if display_style != 0 {
                    format!("Err({})", err_s)
                } else {
                    format!("{{\"Err\":[{}]}}", json_escape_string(&err_s))
                };
                (0, s)
            } else {
                // SAFETY: the offset is within bounds of `pack`'s allocation; the handle value is valid for the target type
                let mh = unsafe { *pack.add(1) } as MapHandle;
                let jp = mimi_map_to_json_product_i64(mh, arity, display_style);
                // SAFETY: `jp` is a valid null-terminated C string returned by a Mimi allocation function
                let map_s = unsafe { cstr_to_string(jp) };
                if !jp.is_null() {
                    mimi_free(jp as *mut _);
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
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_map_result_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
            // SAFETY: `c_obj` was allocated by `alloc_c_string` (which uses `libc::malloc`) and the null check above ensures it is a valid pointer for deallocation
            mimi_free(c_obj as *mut _);
        }
        // SAFETY: `map_from_handle(handle)` returns a valid non-null pointer to a `MimiMap` instance; the handle was previously created by `mimi_map_new()` and is still alive
        unsafe {
            map_from_handle(handle)
                .inner
                .insert(key, inner as ValueHandle);
        }
    }
    handle
}

/// Map of Map of Result of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_map_result_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        // SAFETY: `inner_json` is a valid null-terminated C string returned by a Mimi allocation function
        let s = unsafe { cstr_to_string(inner_json) };
        if !inner_json.is_null() {
            mimi_free(inner_json as *mut _);
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Set of Map of List of product from JSON.

/// Set of Map of Set of product from JSON.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_set_from_json_map_set_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> SetHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_set_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
            // SAFETY: `c_obj` was allocated by `alloc_c_string` (which uses `libc::malloc`) and the null check above ensures it is a valid pointer for deallocation
            mimi_free(c_obj as *mut _);
        }
        mimi_set_insert(handle, mh as SetValueHandle);
    }
    handle
}

/// Set of Map of Set of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_set_to_json_map_set_product_i64(
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
    // SAFETY: `set_from_handle` is non-null and points to a valid `MimiSet`
    let set = set_from_handle(handle);
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
            // SAFETY: `jp` is a valid null-terminated C string returned by a Mimi allocation function
            let s = unsafe { cstr_to_string(jp) };
            if !jp.is_null() {
                mimi_free(jp as *mut _);
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
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_set_map_list_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
            // SAFETY: `c_arr` was allocated by `alloc_c_string` (which uses `libc::malloc`) and the null check above ensures it is a valid pointer for deallocation
            mimi_free(c_arr as *mut _);
        }
        // SAFETY: `map_from_handle(handle)` returns a valid non-null pointer to a `MimiMap` instance; the handle was previously created by `mimi_map_new()` and is still alive
        unsafe {
            map_from_handle(handle)
                .inner
                .insert(key, set_h as ValueHandle);
        }
    }
    handle
}

/// Map of Set of Map of List of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_set_map_list_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        // SAFETY: `set_json` is a valid null-terminated C string returned by a Mimi allocation function
        let s = unsafe { cstr_to_string(set_json) };
        if !set_json.is_null() {
            mimi_free(set_json as *mut _);
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_set_from_json_map_list_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> SetHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_set_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
            // SAFETY: `c_obj` was allocated by `alloc_c_string` (which uses `libc::malloc`) and the null check above ensures it is a valid pointer for deallocation
            mimi_free(c_obj as *mut _);
        }
        mimi_set_insert(handle, mh as SetValueHandle);
    }
    handle
}

/// Set of Map of List of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_set_to_json_map_list_product_i64(
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
    // SAFETY: `set_from_handle` is non-null and points to a valid `MimiSet`
    let set = set_from_handle(handle);
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
            // SAFETY: `jp` is a valid null-terminated C string returned by a Mimi allocation function
            let s = unsafe { cstr_to_string(jp) };
            if !jp.is_null() {
                mimi_free(jp as *mut _);
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

///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_map_list_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
            // SAFETY: `c_obj` was allocated by `alloc_c_string` (which uses `libc::malloc`) and the null check above ensures it is a valid pointer for deallocation
            mimi_free(c_obj as *mut _);
        }
        // SAFETY: `map_from_handle(handle)` returns a valid non-null pointer to a `MimiMap` instance; the handle was previously created by `mimi_map_new()` and is still alive
        unsafe {
            map_from_handle(handle)
                .inner
                .insert(key, inner as ValueHandle);
        }
    }
    handle
}

/// Map of Map of List of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_map_list_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        // SAFETY: `inner_json` is a valid null-terminated C string returned by a Mimi allocation function
        let s = unsafe { cstr_to_string(inner_json) };
        if !inner_json.is_null() {
            mimi_free(inner_json as *mut _);
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of Map of Option of product from JSON.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_map_option_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
            // SAFETY: `c_obj` was allocated by `alloc_c_string` (which uses `libc::malloc`) and the null check above ensures it is a valid pointer for deallocation
            mimi_free(c_obj as *mut _);
        }
        // SAFETY: `map_from_handle(handle)` returns a valid non-null pointer to a `MimiMap` instance; the handle was previously created by `mimi_map_new()` and is still alive
        unsafe {
            map_from_handle(handle)
                .inner
                .insert(key, inner as ValueHandle);
        }
    }
    handle
}

/// Map of Map of Option of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_map_option_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        // SAFETY: `inner_json` is a valid null-terminated C string returned by a Mimi allocation function
        let s = unsafe { cstr_to_string(inner_json) };
        if !inner_json.is_null() {
            mimi_free(inner_json as *mut _);
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Set of Option of Map of product from JSON.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_set_from_json_option_map_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> SetHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_set_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
        // SAFETY: `size` is a valid, non-negative allocation size
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            break;
        }
        if bytes[i] == b'n' && i + 4 <= bytes.len() && &bytes[i..i + 4] == b"null" {
            i += 4;
            // SAFETY: `pack` was allocated by `libc::malloc(16)` at line 8161 and confirmed non-null at line 8162; `pack` and `pack.add(1)` point to valid, aligned memory
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
                // SAFETY: `c_obj` was allocated by `alloc_c_string` (which uses `libc::malloc`) and the null check above ensures it is a valid pointer for deallocation
                mimi_free(c_obj as *mut _);
            }
            // SAFETY: `pack` was allocated by `libc::malloc(16)` at line 8161 and confirmed non-null at line 8162; `pack` and `pack.add(1)` point to valid, aligned memory
            unsafe {
                *pack = 1;
                *pack.add(1) = mh as i64;
            }
        } else {
            // SAFETY: `pack` was allocated by `libc::malloc(16)` at line 8161 and confirmed non-null at line 8162; null check passed before reaching this branch; `pack` is a valid `libc::malloc` allocation
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
pub unsafe extern "C" fn mimi_set_to_json_option_map_product_i64(
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
    // SAFETY: `set_from_handle` is non-null and points to a valid `MimiSet`
    let set = set_from_handle(handle);
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
            // SAFETY: `pack` points to a valid, properly aligned value
            let disc = unsafe { *pack };
            let s = if disc == 0 {
                if display_style != 0 {
                    String::from("None()")
                } else {
                    String::from("\"None\"")
                }
            } else {
                // SAFETY: the offset is within bounds of `pack`'s allocation; the handle value is valid for the target type
                let mh = unsafe { *pack.add(1) } as MapHandle;
                let jp = mimi_map_to_json_product_i64(mh, arity, display_style);
                // SAFETY: `jp` is a valid null-terminated C string returned by a Mimi allocation function
                let map_s = unsafe { cstr_to_string(jp) };
                if !jp.is_null() {
                    mimi_free(jp as *mut _);
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
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_map_set_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
            // SAFETY: `c_obj` was allocated by `alloc_c_string` (which uses `libc::malloc`) and the null check above ensures it is a valid pointer for deallocation
            mimi_free(c_obj as *mut _);
        }
        // SAFETY: `map_from_handle(handle)` returns a valid non-null pointer to a `MimiMap` instance; the handle was previously created by `mimi_map_new()` and is still alive
        unsafe {
            map_from_handle(handle)
                .inner
                .insert(key, inner as ValueHandle);
        }
    }
    handle
}

/// Map of Map of Set of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_map_set_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        // SAFETY: `inner_json` is a valid null-terminated C string returned by a Mimi allocation function
        let s = unsafe { cstr_to_string(inner_json) };
        if !inner_json.is_null() {
            mimi_free(inner_json as *mut _);
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Set of Map of product from JSON.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_set_from_json_map_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> SetHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_set_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
            // SAFETY: `c_obj` was allocated by `alloc_c_string` and is non-null (checked above); `mimi_free` is the matching deallocation (mimi_alloc/alloc_c_string path)
            mimi_free(c_obj as *mut _);
        }
        mimi_set_insert(handle, mh as SetValueHandle);
    }
    handle
}

/// Set of Map of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_set_to_json_map_product_i64(
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
    // SAFETY: `set_from_handle` is non-null and points to a valid `MimiSet`
    let set = set_from_handle(handle);
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
            // SAFETY: `jp` is a valid null-terminated C string returned by a Mimi allocation function
            let s = unsafe { cstr_to_string(jp) };
            if !jp.is_null() {
                mimi_free(jp as *mut _);
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
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_from_json_set_map_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> *mut MimiList {
    // audit-wave1 CRITICAL (allocator mismatch): Box allocation + explicit
    // element_kind (elements are SetHandles → Set, owned by the registry).
    let empty = || Box::into_raw(Box::new(MimiList::new_with_kind(ListElementKind::Set)));
    if json.is_null() || arity <= 0 || arity > 16 {
        return empty();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
            // SAFETY: `std::mem::size_of::<MimiList>(` is a valid, non-negative allocation size; handles null return below
            mimi_free(c_arr as *mut _);
        }
        handles.push(set_h as i64);
    }
    let data_size = handles.len() * 8;
    let data = if data_size == 0 {
        std::ptr::null_mut()
    } else {
        // SAFETY: `data_size` is positive; result is checked for null below.
        unsafe { libc::malloc(data_size) as *mut i64 }
    };
    if data_size > 0 && data.is_null() {
        return empty();
    }
    if !data.is_null() {
        // SAFETY: `data` was allocated by `libc::malloc(data_size)` with sufficient capacity for `handles.len()` elements; `handles.as_ptr()` and `data` are both valid, properly aligned, and non-overlapping pointers
        unsafe {
            std::ptr::copy_nonoverlapping(handles.as_ptr(), data, handles.len());
        }
    }
    // audit-wave1 CRITICAL: Box-allocated struct + explicit element_kind
    // (elements are SetHandles).
    Box::into_raw(Box::new(MimiList::with_data(
        data as *mut *mut std::ffi::c_char,
        handles.len() as i64,
        true,
        ListElementKind::Set,
    )))
}

/// List of Set of Map of product Display/JSON.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_set_map_product_to_json(
    list: *const MimiList,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if list.is_null() || arity <= 0 || arity > 16 {
        return alloc_c_string("[]");
    }
    // SAFETY: `list` is non-null and points to a valid `MimiList`
    let lst = unsafe { &*list.cast::<MimiListAbiPrefix>() };
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
        // SAFETY: `lst.data` points to a valid, properly aligned value
        let h = unsafe { *(lst.data as *const i64).offset(i) };
        let set_json = mimi_set_to_json_map_product_i64(h as SetHandle, arity, display_style);
        // SAFETY: `set_json` is a valid null-terminated C string returned by a Mimi allocation function
        let s = unsafe { cstr_to_string(set_json) };
        if !set_json.is_null() {
            mimi_free(set_json as *mut _);
        }
        parts.push(s);
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

/// List of Set of product from JSON.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_from_json_set_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> *mut MimiList {
    // audit-wave1 CRITICAL (allocator mismatch): Box allocation + explicit
    // element_kind (elements are SetHandles → Set, owned by the registry).
    let empty = || Box::into_raw(Box::new(MimiList::new_with_kind(ListElementKind::Set)));
    if json.is_null() || arity <= 0 || arity > 16 {
        return empty();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
            // SAFETY: `std::mem::size_of::<MimiList>(` is a valid, non-negative allocation size; handles null return below
            mimi_free(c_arr as *mut _);
        }
        handles.push(set_h as i64);
    }
    let data_size = handles.len() * 8;
    let data = if data_size == 0 {
        std::ptr::null_mut()
    } else {
        // SAFETY: `data_size` is positive; result is checked for null below.
        unsafe { libc::malloc(data_size) as *mut i64 }
    };
    if data_size > 0 && data.is_null() {
        return empty();
    }
    if !data.is_null() {
        // SAFETY: `data` was allocated by `libc::malloc(data_size)` with sufficient capacity for `handles.len()` elements; `handles.as_ptr()` and `data` are both valid, properly aligned, and non-overlapping pointers
        unsafe {
            std::ptr::copy_nonoverlapping(handles.as_ptr(), data, handles.len());
        }
    }
    // audit-wave1 CRITICAL: Box-allocated struct + explicit element_kind
    // (elements are SetHandles).
    Box::into_raw(Box::new(MimiList::with_data(
        data as *mut *mut std::ffi::c_char,
        handles.len() as i64,
        true,
        ListElementKind::Set,
    )))
}

/// Set of List of product from JSON array of list-of-product arrays.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_set_from_json_list_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> SetHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_set_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
            // SAFETY: `c_wrap` was allocated by `alloc_c_string` and is non-null (checked above); `mimi_free` is the matching deallocation (mimi_alloc/alloc_c_string path)
            mimi_free(c_wrap as *mut _);
        }
        if tmp != 0 {
            // SAFETY: `map_from_handle` is non-null and points to a valid map instance
            let m = map_from_handle(tmp);
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
pub unsafe extern "C" fn mimi_set_to_json_list_product_i64(
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
    // SAFETY: `set_from_handle` is non-null and points to a valid `MimiSet`
    let set = set_from_handle(handle);
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
                // SAFETY: `tmp` was just created by `mimi_map_new()` and verified non-zero; `map_from_handle(tmp)` returns a valid `*mut MimiMap`
                unsafe {
                    map_from_handle(tmp)
                        .inner
                        .insert("_".into(), *h as ValueHandle);
                }
                let jp = mimi_map_to_json_list_product_i64(tmp, arity, display_style);
                // SAFETY: `jp` is a valid null-terminated C string returned by a Mimi allocation function
                let s = unsafe { cstr_to_string(jp) };
                unsafe {
                    mimi_map_destroy(tmp);
                }
                if !jp.is_null() {
                    mimi_free(jp as *mut _);
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
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_from_json_map_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> *mut MimiList {
    // audit-wave1 CRITICAL (allocator mismatch): Box allocation + explicit
    // element_kind (elements are MapHandles → Map, owned by the registry).
    let empty = || Box::into_raw(Box::new(MimiList::new_with_kind(ListElementKind::Map)));
    if json.is_null() || arity <= 0 || arity > 16 {
        return empty();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
            // SAFETY: `std::mem::size_of::<MimiList>(` is a valid, non-negative allocation size; handles null return below
            mimi_free(c_obj as *mut _);
        }
        handles.push(mh as i64);
    }
    let data_size = handles.len() * 8;
    let data = if data_size == 0 {
        std::ptr::null_mut()
    } else {
        // SAFETY: `data_size` is positive; result is checked for null below.
        unsafe { libc::malloc(data_size) as *mut i64 }
    };
    if data_size > 0 && data.is_null() {
        return empty();
    }
    if !data.is_null() {
        // SAFETY: `data` was allocated by `libc::malloc(data_size)` with sufficient capacity for `handles.len()` elements; `handles.as_ptr()` and `data` are both valid, properly aligned, and non-overlapping pointers
        unsafe {
            std::ptr::copy_nonoverlapping(handles.as_ptr(), data, handles.len());
        }
    }
    // audit-wave1 CRITICAL: Box-allocated struct + explicit element_kind
    // (elements are MapHandles).
    Box::into_raw(Box::new(MimiList::with_data(
        data as *mut *mut std::ffi::c_char,
        handles.len() as i64,
        true,
        ListElementKind::Map,
    )))
}

/// List of Map of product Display/JSON.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_map_product_to_json(
    list: *const MimiList,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if list.is_null() || arity <= 0 || arity > 16 {
        return alloc_c_string("[]");
    }
    // SAFETY: `list` is non-null and points to a valid `MimiList`
    let lst = unsafe { &*list.cast::<MimiListAbiPrefix>() };
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
        // SAFETY: `lst.data` points to a valid, properly aligned value
        let h = unsafe { *(lst.data as *const i64).offset(i) };
        let map_json = mimi_map_to_json_product_i64(h as MapHandle, arity, display_style);
        // SAFETY: `map_json` is a valid null-terminated C string returned by a Mimi allocation function
        let s = unsafe { cstr_to_string(map_json) };
        if !map_json.is_null() {
            mimi_free(map_json as *mut _);
        }
        parts.push(s);
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

/// Map of Set of List of Map of product from JSON.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_set_list_map_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
            // SAFETY: `c_arr` was allocated by `alloc_c_string` and is non-null (checked above); `mimi_free` is the matching deallocation (mimi_alloc/alloc_c_string path)
            mimi_free(c_arr as *mut _);
        }
        // SAFETY: `handle` is a valid `MapHandle` (verified non-zero at function entry); `map_from_handle(handle)` returns a valid `*mut MimiMap`
        unsafe {
            map_from_handle(handle)
                .inner
                .insert(key, set_h as ValueHandle);
        }
    }
    handle
}

/// Map of Set of List of Map of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_set_list_map_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        // SAFETY: `set_json` is a valid null-terminated C string returned by a Mimi allocation function
        let s = unsafe { cstr_to_string(set_json) };
        if !set_json.is_null() {
            mimi_free(set_json as *mut _);
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of List of Set of Map of product from JSON.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_list_set_map_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
            // SAFETY: `c_arr` was allocated by `alloc_c_string` and is non-null (checked above); `mimi_free` is the matching deallocation (mimi_alloc/alloc_c_string path)
            mimi_free(c_arr as *mut _);
        }
        // SAFETY: `handle` is a valid `MapHandle` (verified non-zero at function entry); `map_from_handle(handle)` returns a valid `*mut MimiMap`
        let vh = list_ptr as ValueHandle;
        // SAFETY: `map_from_handle(handle)` returns a valid, properly aligned pointer; `key` is a valid `String`
        unsafe {
            let mut map_ptr = map_from_handle(handle);
            (*map_ptr).inner.insert(key, vh);
            // §10-#35: this list_ptr was malloc'd by the builder — register so
            // destroy() can reclaim its base (inner object handles are
            // intentional bounded leaks, see mimi_map_destroy comment).
            (*map_ptr).owned.insert(vh, MapOwnedValueKind::ListObject);
        }
        MAP_OWNED_VALUE_BALANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    handle
}

/// Map of List of Set of Map of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_list_set_map_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        let json_ptr = unsafe { mimi_list_set_map_product_to_json(list_ptr, arity, display_style) };
        // SAFETY: `json_ptr` is a valid null-terminated C string returned by a Mimi allocation function
        let s = unsafe { cstr_to_string(json_ptr) };
        if !json_ptr.is_null() {
            mimi_free(json_ptr as *mut _);
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of Set of Map of product from JSON.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_set_map_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
            // SAFETY: `c_arr` was allocated by `alloc_c_string` and is non-null (checked above); `mimi_free` is the matching deallocation (mimi_alloc/alloc_c_string path)
            mimi_free(c_arr as *mut _);
        }
        // SAFETY: `handle` is a valid `MapHandle` (verified non-zero at function entry); `map_from_handle(handle)` returns a valid `*mut MimiMap`
        unsafe {
            map_from_handle(handle)
                .inner
                .insert(key, set_h as ValueHandle);
        }
    }
    handle
}

/// Map of Set of Map of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_set_map_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        // SAFETY: `set_json` is a valid null-terminated C string returned by a Mimi allocation function
        let s = unsafe { cstr_to_string(set_json) };
        if !set_json.is_null() {
            mimi_free(set_json as *mut _);
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of List of Map of product from JSON.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_list_map_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
            // SAFETY: `c_arr` was allocated by `alloc_c_string` and is non-null (checked above); `mimi_free` is the matching deallocation (mimi_alloc/alloc_c_string path)
            mimi_free(c_arr as *mut _);
        }
        // SAFETY: `handle` is a valid `MapHandle` (verified non-zero at function entry); `map_from_handle(handle)` returns a valid `*mut MimiMap`
        let vh = list_ptr as ValueHandle;
        // SAFETY: `map_from_handle(handle)` returns a valid, properly aligned pointer; `key` is a valid `String`
        unsafe {
            let mut map_ptr = map_from_handle(handle);
            (*map_ptr).inner.insert(key, vh);
            // §10-#35: this list_ptr was malloc'd by the builder — register so
            // destroy() can reclaim its base (inner object handles are
            // intentional bounded leaks, see mimi_map_destroy comment).
            (*map_ptr).owned.insert(vh, MapOwnedValueKind::ListObject);
        }
        MAP_OWNED_VALUE_BALANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    handle
}

/// Map of List of Map of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_list_map_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        let json_ptr = unsafe { mimi_list_map_product_to_json(list_ptr, arity, display_style) };
        // SAFETY: `json_ptr` is a valid null-terminated C string returned by a Mimi allocation function
        let s = unsafe { cstr_to_string(json_ptr) };
        if !json_ptr.is_null() {
            mimi_free(json_ptr as *mut _);
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of Set of List of product from JSON.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_set_list_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
            // SAFETY: `c_arr` was allocated by `alloc_c_string` and is non-null (checked above); `mimi_free` is the matching deallocation (mimi_alloc/alloc_c_string path)
            mimi_free(c_arr as *mut _);
        }
        // SAFETY: `handle` is a valid `MapHandle` (verified non-zero at function entry); `map_from_handle(handle)` returns a valid `*mut MimiMap`
        unsafe {
            map_from_handle(handle)
                .inner
                .insert(key, set_h as ValueHandle);
        }
    }
    handle
}

/// Map of Set of List of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_set_list_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        // SAFETY: `json_ptr` is a valid null-terminated C string returned by a Mimi allocation function
        let s = unsafe { cstr_to_string(json_ptr) };
        if !json_ptr.is_null() {
            mimi_free(json_ptr as *mut _);
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of List of Set of Result of product from JSON.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_list_set_result_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
            // SAFETY: `c_arr` was allocated by `alloc_c_string` and is non-null (checked above); `mimi_free` is the matching deallocation (mimi_alloc/alloc_c_string path)
            mimi_free(c_arr as *mut _);
        }
        // SAFETY: `handle` is a valid `MapHandle` (verified non-zero at function entry); `map_from_handle(handle)` returns a valid `*mut MimiMap`
        let vh = list_ptr as ValueHandle;
        // SAFETY: `map_from_handle(handle)` returns a valid, properly aligned pointer; `key` is a valid `String`
        unsafe {
            let mut map_ptr = map_from_handle(handle);
            (*map_ptr).inner.insert(key, vh);
            // §10-#35: this list_ptr was malloc'd by the builder — register so
            // destroy() can reclaim its base (inner object handles are
            // intentional bounded leaks, see mimi_map_destroy comment).
            (*map_ptr).owned.insert(vh, MapOwnedValueKind::ListObject);
        }
        MAP_OWNED_VALUE_BALANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    handle
}

/// Map of List of Set of Result of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_list_set_result_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        let json_ptr =
            unsafe { mimi_list_set_result_product_to_json(list_ptr, arity, display_style) };
        // SAFETY: `json_ptr` is a valid null-terminated C string returned by a Mimi allocation function
        let s = unsafe { cstr_to_string(json_ptr) };
        if !json_ptr.is_null() {
            mimi_free(json_ptr as *mut _);
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of List of Set of Option of product from JSON.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_list_set_option_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
            // SAFETY: `c_arr` was allocated by `alloc_c_string` and is non-null (checked above); `mimi_free` is the matching deallocation (mimi_alloc/alloc_c_string path)
            mimi_free(c_arr as *mut _);
        }
        // SAFETY: `handle` is a valid `MapHandle` (verified non-zero at function entry); `map_from_handle(handle)` returns a valid `*mut MimiMap`
        let vh = list_ptr as ValueHandle;
        // SAFETY: `map_from_handle(handle)` returns a valid, properly aligned pointer; `key` is a valid `String`
        unsafe {
            let mut map_ptr = map_from_handle(handle);
            (*map_ptr).inner.insert(key, vh);
            // §10-#35: this list_ptr was malloc'd by the builder — register so
            // destroy() can reclaim its base (inner object handles are
            // intentional bounded leaks, see mimi_map_destroy comment).
            (*map_ptr).owned.insert(vh, MapOwnedValueKind::ListObject);
        }
        MAP_OWNED_VALUE_BALANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    handle
}

/// Map of List of Set of Option of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_list_set_option_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        let json_ptr =
            unsafe { mimi_list_set_option_product_to_json(list_ptr, arity, display_style) };
        // SAFETY: `json_ptr` is a valid null-terminated C string returned by a Mimi allocation function
        let s = unsafe { cstr_to_string(json_ptr) };
        if !json_ptr.is_null() {
            mimi_free(json_ptr as *mut _);
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of List of Set of product from JSON.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_list_set_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
            // SAFETY: `c_arr` was allocated by `alloc_c_string` and is non-null (checked above); `mimi_free` is the matching deallocation (mimi_alloc/alloc_c_string path)
            mimi_free(c_arr as *mut _);
        }
        // SAFETY: `handle` is a valid `MapHandle` (verified non-zero at function entry); `map_from_handle(handle)` returns a valid `*mut MimiMap`
        let vh = list_ptr as ValueHandle;
        // SAFETY: `map_from_handle(handle)` returns a valid, properly aligned pointer; `key` is a valid `String`
        unsafe {
            let mut map_ptr = map_from_handle(handle);
            (*map_ptr).inner.insert(key, vh);
            // §10-#35: this list_ptr was malloc'd by the builder — register so
            // destroy() can reclaim its base (inner object handles are
            // intentional bounded leaks, see mimi_map_destroy comment).
            (*map_ptr).owned.insert(vh, MapOwnedValueKind::ListObject);
        }
        MAP_OWNED_VALUE_BALANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    handle
}

/// Map of List of Set of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_list_set_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
            unsafe { mimi_list_set_product_to_string(list_ptr, arity) }
        } else {
            unsafe { mimi_list_set_product_to_json(list_ptr, arity) }
        };
        // SAFETY: `json_ptr` is a valid null-terminated C string returned by a Mimi allocation function
        let s = unsafe { cstr_to_string(json_ptr) };
        if !json_ptr.is_null() {
            mimi_free(json_ptr as *mut _);
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// List of Set of Option of product from JSON.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_from_json_set_option_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> *mut MimiList {
    // audit-wave1 CRITICAL (allocator mismatch): Box allocation + explicit
    // element_kind (elements are SetHandles → Set, owned by the registry).
    let empty = || Box::into_raw(Box::new(MimiList::new_with_kind(ListElementKind::Set)));
    if json.is_null() || arity <= 0 || arity > 16 {
        return empty();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
            // SAFETY: `std::mem::size_of::<MimiList>(` is a valid, non-negative allocation size; handles null return below
            mimi_free(c_arr as *mut _);
        }
        handles.push(set_h as i64);
    }
    let data_size = handles.len() * 8;
    let data = if data_size == 0 {
        std::ptr::null_mut()
    } else {
        // SAFETY: `data_size` is positive; result is checked for null below.
        unsafe { libc::malloc(data_size) as *mut i64 }
    };
    if data_size > 0 && data.is_null() {
        return empty();
    }
    if !data.is_null() {
        // SAFETY: `data` was allocated by `libc::malloc(data_size)` with sufficient capacity for `handles.len()` elements; `handles.as_ptr()` and `data` are both valid, properly aligned, and non-overlapping pointers
        unsafe {
            std::ptr::copy_nonoverlapping(handles.as_ptr(), data, handles.len());
        }
    }
    // audit-wave1 CRITICAL: Box-allocated struct + explicit element_kind
    // (elements are SetHandles).
    Box::into_raw(Box::new(MimiList::with_data(
        data as *mut *mut std::ffi::c_char,
        handles.len() as i64,
        true,
        ListElementKind::Set,
    )))
}

/// List of Set of Option of product Display/JSON.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_set_option_product_to_json(
    list: *const MimiList,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if list.is_null() || arity <= 0 || arity > 16 {
        return alloc_c_string("[]");
    }
    // SAFETY: `list` is non-null and points to a valid `MimiList`
    let lst = unsafe { &*list.cast::<MimiListAbiPrefix>() };
    if lst.data.is_null() || lst.len <= 0 {
        return alloc_c_string("[]");
    }
    // batch4/05 P2-4: cap list length like sibling serializers.
    if lst.len < 0 || lst.len > 1_000_000 {
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
        // SAFETY: `lst.data` points to a valid, properly aligned value
        let h = unsafe { *(lst.data as *const i64).offset(i) };
        let set_json = mimi_set_to_json_option_product_i64(h as SetHandle, arity, display_style);
        // SAFETY: `set_json` is a valid null-terminated C string returned by a Mimi allocation function
        let s = unsafe { cstr_to_string(set_json) };
        if !set_json.is_null() {
            mimi_free(set_json as *mut _);
        }
        parts.push(s);
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

/// List of Set of Result of product from JSON.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_from_json_set_result_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> *mut MimiList {
    // audit-wave1 CRITICAL (allocator mismatch): Box allocation + explicit
    // element_kind (elements are SetHandles → Set, owned by the registry).
    let empty = || Box::into_raw(Box::new(MimiList::new_with_kind(ListElementKind::Set)));
    if json.is_null() || arity <= 0 || arity > 16 {
        return empty();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
            // SAFETY: `std::mem::size_of::<MimiList>(` is a valid, non-negative allocation size; handles null return below
            mimi_free(c_arr as *mut _);
        }
        handles.push(set_h as i64);
    }
    let data_size = handles.len() * 8;
    let data = if data_size == 0 {
        std::ptr::null_mut()
    } else {
        // SAFETY: `data_size` is positive; result is checked for null below.
        unsafe { libc::malloc(data_size) as *mut i64 }
    };
    if data_size > 0 && data.is_null() {
        return empty();
    }
    if !data.is_null() {
        // SAFETY: `data` was allocated by `libc::malloc(data_size)` with sufficient capacity for `handles.len()` elements; `handles.as_ptr()` and `data` are both valid, properly aligned, and non-overlapping pointers
        unsafe {
            std::ptr::copy_nonoverlapping(handles.as_ptr(), data, handles.len());
        }
    }
    // audit-wave1 CRITICAL: Box-allocated struct + explicit element_kind
    // (elements are SetHandles).
    Box::into_raw(Box::new(MimiList::with_data(
        data as *mut *mut std::ffi::c_char,
        handles.len() as i64,
        true,
        ListElementKind::Set,
    )))
}

/// List of Set of Result of product Display/JSON.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_set_result_product_to_json(
    list: *const MimiList,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if list.is_null() || arity <= 0 || arity > 16 {
        return alloc_c_string("[]");
    }
    // SAFETY: `list` is non-null and points to a valid `MimiList`
    let lst = unsafe { &*list.cast::<MimiListAbiPrefix>() };
    if lst.data.is_null() || lst.len <= 0 {
        return alloc_c_string("[]");
    }
    // batch4/05 P2-4: cap list length like sibling serializers.
    if lst.len < 0 || lst.len > 1_000_000 {
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
        // SAFETY: `lst.data` points to a valid, properly aligned value
        let h = unsafe { *(lst.data as *const i64).offset(i) };
        let set_json = mimi_set_to_json_result_product_i64(h as SetHandle, arity, display_style);
        // SAFETY: `set_json` is a valid null-terminated C string returned by a Mimi allocation function
        let s = unsafe { cstr_to_string(set_json) };
        if !set_json.is_null() {
            mimi_free(set_json as *mut _);
        }
        parts.push(s);
    }
    parts.push(String::from("]"));
    alloc_c_string(&parts.join(""))
}

/// List of Result of Option of product from JSON.
/// Element pack: `{i64 res_disc, i64 opt_pack_or_err}` where opt_pack is option product heap.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_from_json_result_option_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> *mut MimiList {
    // audit-wave1 CRITICAL (allocator mismatch): Box allocation + explicit
    // element_kind (elements are malloc'd result-option packs → Record).
    let empty = || Box::into_raw(Box::new(MimiList::new_with_kind(ListElementKind::Record)));
    if json.is_null() || arity <= 0 || arity > 16 {
        return empty();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
            // SAFETY: `c_wrap` was allocated by `alloc_c_string` and is non-null (checked above); `mimi_free` is the matching deallocation (mimi_alloc/alloc_c_string path)
            mimi_free(c_wrap as *mut _);
        }
        let mut h: i64 = 0;
        if tmp != 0 {
            // SAFETY: `map_from_handle` is non-null and points to a valid map instance.
            let mut m = map_from_handle(tmp);
            if let Some(&v) = m.inner.values().next() {
                h = v as i64;
                // The list owns this record pointer and frees it via
                // mimi_list_free. Remove it from the temporary map's owned
                // registry so destroying the map does not double-free it.
                m.owned.remove(&v);
            }
            m.inner.clear();
        }
        if tmp != 0 {
            unsafe {
                mimi_map_destroy(tmp);
            }
        }
        handles.push(h);
    }
    let data_size = handles.len() * 8;
    let data = if data_size == 0 {
        std::ptr::null_mut()
    } else {
        // SAFETY: `data_size` is positive; result is checked for null below.
        unsafe { libc::malloc(data_size) as *mut i64 }
    };
    if data_size > 0 && data.is_null() {
        return empty();
    }
    if !data.is_null() {
        // SAFETY: `data` was allocated by `libc::malloc(data_size)` with sufficient capacity for `handles.len()` elements; `handles.as_ptr()` and `data` are both valid, properly aligned, and non-overlapping pointers
        unsafe {
            std::ptr::copy_nonoverlapping(handles.as_ptr(), data, handles.len());
        }
    }
    // audit-wave1 CRITICAL: Box-allocated struct + explicit element_kind
    // (elements are malloc'd result-option packs → Record).
    Box::into_raw(Box::new(MimiList::with_data(
        data as *mut *mut std::ffi::c_char,
        handles.len() as i64,
        true,
        ListElementKind::Record,
    )))
}

/// List of Result of Option of product Display/JSON.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_result_option_product_to_json(
    list: *const MimiList,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if list.is_null() || arity <= 0 || arity > 16 {
        return alloc_c_string("[]");
    }
    // SAFETY: `list` is non-null and points to a valid `MimiList`
    let lst = unsafe { &*list.cast::<MimiListAbiPrefix>() };
    if lst.data.is_null() || lst.len <= 0 {
        return alloc_c_string("[]");
    }
    // batch4/05 P2-4: cap list length like sibling serializers.
    if lst.len < 0 || lst.len > 1_000_000 {
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
        // SAFETY: `lst.data` points to a valid, properly aligned value
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
            // SAFETY: `tmp` was just created by `mimi_map_new()` and verified non-zero; `map_from_handle(tmp)` returns a valid `*mut MimiMap`
            unsafe {
                map_from_handle(tmp)
                    .inner
                    .insert("_".into(), h as ValueHandle);
            }
            let json_ptr = mimi_map_to_json_result_option_product_i64(tmp, arity, display_style);
            // SAFETY: `json_ptr` is a valid null-terminated C string returned by a Mimi allocation function
            let s = unsafe { cstr_to_string(json_ptr) };
            unsafe {
                mimi_map_destroy(tmp);
            }
            if !json_ptr.is_null() {
                mimi_free(json_ptr as *mut _);
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
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_list_result_option_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
            // SAFETY: `c_arr` was allocated by `alloc_c_string` and is non-null (checked above); `mimi_free` is the matching deallocation (mimi_alloc/alloc_c_string path)
            mimi_free(c_arr as *mut _);
        }
        // SAFETY: `handle` is a valid `MapHandle` (verified non-zero at function entry); `map_from_handle(handle)` returns a valid `*mut MimiMap`
        let vh = list_ptr as ValueHandle;
        // SAFETY: `map_from_handle(handle)` returns a valid, properly aligned pointer; `key` is a valid `String`
        unsafe {
            let mut map_ptr = map_from_handle(handle);
            (*map_ptr).inner.insert(key, vh);
            // §10-#35: this list_ptr was malloc'd by the builder — register so
            // destroy() can reclaim its base (inner object handles are
            // intentional bounded leaks, see mimi_map_destroy comment).
            (*map_ptr).owned.insert(vh, MapOwnedValueKind::ListObject);
        }
        MAP_OWNED_VALUE_BALANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    handle
}

/// Map of List of Result of Option of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_list_result_option_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        let json_ptr =
            unsafe { mimi_list_result_option_product_to_json(list_ptr, arity, display_style) };
        // SAFETY: `json_ptr` is a valid null-terminated C string returned by a Mimi allocation function
        let s = unsafe { cstr_to_string(json_ptr) };
        if !json_ptr.is_null() {
            mimi_free(json_ptr as *mut _);
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of Result of Option of List of product from JSON.
/// Pack: `{i64 disc, i64 opt_or_err}` where Ok opt pack is `{i64 opt_disc, i64 list_handle}`.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_result_option_list_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
        // SAFETY: `size` is a valid, non-negative allocation size
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            continue;
        }
        if bytes[i] != b'{' {
            // SAFETY: `pack` was allocated by `libc::malloc(16)` and verified non-null; `libc::free` is the matching deallocation
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
            // SAFETY: `pack` was allocated by `libc::malloc(16)` and verified non-null; `pack.add(1)` is within the 16-byte allocation (2 × i64 is exactly 16 bytes)
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
            // SAFETY: `size` is a valid, non-negative allocation size
            let opt_pack = unsafe { libc::malloc(16) as *mut i64 };
            if opt_pack.is_null() {
                unsafe {
                    libc::free(pack as *mut _);
                }
                continue;
            }
            if val == "null" {
                // SAFETY: `opt_pack` is a valid, non-null pointer from the `libc::malloc(16)` allocation at line 11075 (null-checked at 11076)
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
                    // SAFETY: `c_wrap` is non-null and points to memory allocated by `alloc_c_string` (null-checked at line 11092)
                    mimi_free(c_wrap as *mut _);
                }
                let mut list_h: i64 = 0;
                if tmp != 0 {
                    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
                    let m = map_from_handle(tmp);
                    if let Some(v) = m.inner.values().next() {
                        list_h = *v as i64;
                    }
                }
                // SAFETY: `opt_pack` is a valid, non-null pointer from the `libc::malloc(16)` allocation at line 11075 (null-checked at 11076)
                unsafe {
                    *opt_pack = 1;
                    *opt_pack.add(1) = list_h;
                }
            }
            // SAFETY: `pack` is a valid, non-null pointer from the `libc::malloc(16)` allocation at line 10979 (null-checked at 10980)
            unsafe {
                *pack = 1;
                *pack.add(1) = opt_pack as i64;
            }
        }
        // SAFETY: `handle` is non-zero (validated at function entry line 10937) so `map_from_handle(handle)` returns a valid pointer
        let vh = pack as ValueHandle;
        // SAFETY: `map_from_handle(handle)` returns a valid, properly aligned pointer; `key` is a valid `String`
        unsafe {
            let mut map_ptr = map_from_handle(handle);
            (*map_ptr).inner.insert(key, vh);
            // §10-#35: this pack was malloc'd by the builder — register so
            // destroy() can reclaim its base (inner object handles are
            // intentional bounded leaks, see mimi_map_destroy comment).
            (*map_ptr)
                .owned
                .insert(vh, MapOwnedValueKind::PackErrCString);
        }
        MAP_OWNED_VALUE_BALANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    handle
}

/// Map of Result of Option of List of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_result_option_list_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        // SAFETY: `base` points to a valid, properly aligned value.
        if !pages_mapped(base as usize, 16) {
            parts.push(String::from("null"));
            continue;
        }
        let disc = unsafe { *base };
        if disc == 0 {
            // SAFETY: `err_ptr` is a valid null-terminated C string from a prior allocation
            let err_ptr = unsafe { *base.add(1) } as *const std::ffi::c_char;
            let err_s = if err_ptr.is_null() {
                String::new()
            } else {
                // SAFETY: `err_ptr` is a valid null-terminated C string returned by a Mimi allocation function
                unsafe { cstr_to_string(err_ptr) }
            };
            if display_style != 0 {
                parts.push(format!("Err({})", err_s));
            } else {
                parts.push(format!("{{\"Err\":[{}]}}", json_escape_string(&err_s)));
            }
        } else {
            // SAFETY: the offset is within bounds of `base`'s allocated buffer
            let opt_h = unsafe { *base.add(1) } as *const i64;
            if opt_h.is_null() {
                if display_style != 0 {
                    parts.push(String::from("Ok(None())"));
                } else {
                    parts.push(String::from("{\"Ok\":[\"None\"]}"));
                }
            } else {
                // SAFETY: `opt_h` points to a valid, properly aligned value
                let opt_disc = unsafe { *opt_h };
                if opt_disc == 0 {
                    if display_style != 0 {
                        parts.push(String::from("Ok(None())"));
                    } else {
                        parts.push(String::from("{\"Ok\":[\"None\"]}"));
                    }
                } else {
                    // SAFETY: the offset is within bounds of `opt_h`'s allocated buffer
                    let list_ptr = unsafe { *opt_h.add(1) } as *const u8;
                    let tmp = mimi_map_new();
                    if tmp != 0 && !list_ptr.is_null() {
                        unsafe {
                            map_from_handle(tmp)
                                .inner
                                .insert("_".into(), list_ptr as ValueHandle);
                        }
                        let json_ptr = mimi_map_to_json_list_product_i64(tmp, arity, display_style);
                        // SAFETY: `json_ptr` is a valid null-terminated C string returned by a Mimi allocation function
                        let s = unsafe { cstr_to_string(json_ptr) };
                        unsafe {
                            mimi_map_destroy(tmp);
                        }
                        if !json_ptr.is_null() {
                            mimi_free(json_ptr as *mut _);
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
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_option_set_list_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
        // SAFETY: `size` is a valid, non-negative allocation size
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            continue;
        }
        if bytes[i] == b'n' && i + 4 <= bytes.len() && &bytes[i..i + 4] == b"null" {
            i += 4;
            // SAFETY: `pack` is a valid, non-null pointer from the `libc::malloc(16)` allocation at line 11289 (null-checked at 11290)
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
                // SAFETY: `pack` is a valid, non-null pointer from the `libc::malloc(16)` allocation at line 11289 (null-checked at 11290)
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
                // SAFETY: `c_arr` is non-null and points to memory allocated by `alloc_c_string` (null-checked at line 11370)
                mimi_free(c_arr as *mut _);
            }
            // SAFETY: `pack` is a valid, non-null pointer from the `libc::malloc(16)` allocation at line 11289 (null-checked at 11290)
            unsafe {
                *pack = 1;
                *pack.add(1) = set_h as i64;
            }
        }
        // SAFETY: `handle` is non-zero (validated at function entry line 11247) so `map_from_handle(handle)` returns a valid pointer
        let vh = pack as ValueHandle;
        // SAFETY: `map_from_handle(handle)` returns a valid, properly aligned pointer; `key` is a valid `String`
        unsafe {
            let mut map_ptr = map_from_handle(handle);
            (*map_ptr).inner.insert(key, vh);
            // §10-#35: this pack was malloc'd by the builder — register so
            // destroy() can reclaim its base (inner object handles are
            // intentional bounded leaks, see mimi_map_destroy comment).
            (*map_ptr).owned.insert(vh, MapOwnedValueKind::Pack);
        }
        MAP_OWNED_VALUE_BALANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    handle
}

/// Map of Option of Set of List of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_option_set_list_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        // SAFETY: `base` points to a valid, properly aligned value.
        if !pages_mapped(base as usize, 16) {
            parts.push(String::from("null"));
            continue;
        }
        let disc = unsafe { *base };
        if disc == 0 {
            if display_style != 0 {
                parts.push(String::from("None()"));
            } else {
                parts.push(String::from("\"None\""));
            }
        } else {
            // SAFETY: the offset is within bounds of `base`'s allocation; the handle value is valid for the target type
            let set_h = unsafe { *base.add(1) } as SetHandle;
            let set_json = mimi_set_to_json_list_product_i64(set_h, arity, display_style);
            // SAFETY: `set_json` is a valid null-terminated C string returned by a Mimi allocation function
            let s = unsafe { cstr_to_string(set_json) };
            if !set_json.is_null() {
                mimi_free(set_json as *mut _);
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
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_option_result_list_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
        // SAFETY: `size` is a valid, non-negative allocation size
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            continue;
        }
        if bytes[i] == b'n' && i + 4 <= bytes.len() && &bytes[i..i + 4] == b"null" {
            i += 4;
            // SAFETY: `pack` is a valid, non-null pointer from the `libc::malloc(16)` allocation at line 11509 (null-checked at 11510)
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
                // SAFETY: `pack` is a valid, non-null pointer from the `libc::malloc(16)` allocation at line 11509 (null-checked at 11510)
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
                // SAFETY: `c_wrap` is non-null and points to memory allocated by `alloc_c_string` (null-checked at line 11557)
                mimi_free(c_wrap as *mut _);
            }
            let mut res_h: i64 = 0;
            if tmp != 0 {
                // SAFETY: `map_from_handle` is non-null and points to a valid map instance
                let m = map_from_handle(tmp);
                if let Some(v) = m.inner.values().next() {
                    res_h = *v as i64;
                }
            }
            // SAFETY: `pack` is a valid, non-null pointer from the `libc::malloc(16)` allocation at line 11509 (null-checked at 11510)
            unsafe {
                *pack = 1;
                *pack.add(1) = res_h;
            }
        }
        // SAFETY: `handle` is non-zero (validated at function entry line 11467) so `map_from_handle(handle)` returns a valid pointer
        let vh = pack as ValueHandle;
        // SAFETY: `map_from_handle(handle)` returns a valid, properly aligned pointer; `key` is a valid `String`
        unsafe {
            let mut map_ptr = map_from_handle(handle);
            (*map_ptr).inner.insert(key, vh);
            // §10-#35: this pack was malloc'd by the builder — register so
            // destroy() can reclaim its base (inner object handles are
            // intentional bounded leaks, see mimi_map_destroy comment).
            (*map_ptr).owned.insert(vh, MapOwnedValueKind::Pack);
        }
        MAP_OWNED_VALUE_BALANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    handle
}

/// Map of Option of Result of List of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_option_result_list_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        // SAFETY: `base` points to a valid, properly aligned value.
        if !pages_mapped(base as usize, 16) {
            parts.push(String::from("null"));
            continue;
        }
        let disc = unsafe { *base };
        if disc == 0 {
            if display_style != 0 {
                parts.push(String::from("None()"));
            } else {
                parts.push(String::from("\"None\""));
            }
        } else {
            // SAFETY: the offset is within bounds of `base`'s allocated buffer
            let res_h = unsafe { *base.add(1) };
            let tmp = mimi_map_new();
            if tmp != 0 {
                unsafe {
                    map_from_handle(tmp)
                        .inner
                        .insert("_".into(), res_h as ValueHandle);
                }
                let json_ptr = mimi_map_to_json_result_list_product_i64(tmp, arity, display_style);
                // SAFETY: `json_ptr` is a valid null-terminated C string returned by a Mimi allocation function
                let s = unsafe { cstr_to_string(json_ptr) };
                unsafe {
                    mimi_map_destroy(tmp);
                }
                if !json_ptr.is_null() {
                    mimi_free(json_ptr as *mut _);
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
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_result_list_set_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
        // SAFETY: `size` is a valid, non-negative allocation size
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            continue;
        }
        if bytes[i] != b'{' {
            // SAFETY: `pack` is a valid, non-null pointer from the `libc::malloc(16)` allocation at line 11721 (null-checked at 11722)
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
            // SAFETY: `pack` is a valid, non-null pointer from the `libc::malloc(16)` allocation at line 11721 (null-checked at 11722)
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
                // SAFETY: `c_arr` is non-null and points to memory allocated by `alloc_c_string` (null-checked at line 11810)
                mimi_free(c_arr as *mut _);
            }
            // SAFETY: `pack` is a valid, non-null pointer from the `libc::malloc(16)` allocation at line 11721 (null-checked at 11722)
            unsafe {
                *pack = 1;
                *pack.add(1) = list_ptr as i64;
            }
        }
        // SAFETY: `handle` is non-zero (validated at function entry line 11679) so `map_from_handle(handle)` returns a valid pointer
        let vh = pack as ValueHandle;
        // SAFETY: `map_from_handle(handle)` returns a valid, properly aligned pointer; `key` is a valid `String`
        unsafe {
            let mut map_ptr = map_from_handle(handle);
            (*map_ptr).inner.insert(key, vh);
            // §10-#35: this pack was malloc'd by the builder — register so
            // destroy() can reclaim its base (inner object handles are
            // intentional bounded leaks, see mimi_map_destroy comment).
            (*map_ptr)
                .owned
                .insert(vh, MapOwnedValueKind::PackErrCString);
        }
        MAP_OWNED_VALUE_BALANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    handle
}

/// Map of Result of List of Set of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_result_list_set_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        // SAFETY: `base` points to a valid, properly aligned value.
        if !pages_mapped(base as usize, 16) {
            parts.push(String::from("null"));
            continue;
        }
        let disc = unsafe { *base };
        if disc == 0 {
            // SAFETY: `err_ptr` is a valid null-terminated C string from a prior allocation
            let err_ptr = unsafe { *base.add(1) } as *const std::ffi::c_char;
            let err_s = if err_ptr.is_null() {
                String::new()
            } else {
                // SAFETY: `err_ptr` is a valid null-terminated C string returned by a Mimi allocation function
                unsafe { cstr_to_string(err_ptr) }
            };
            if display_style != 0 {
                parts.push(format!("Err({})", err_s));
            } else {
                parts.push(format!("{{\"Err\":[{}]}}", json_escape_string(&err_s)));
            }
        } else {
            // SAFETY: the offset is within bounds of `base`'s allocation; the handle value is valid for the target type
            let list_ptr = unsafe { *base.add(1) } as *const MimiList;
            let list_json = if display_style != 0 {
                unsafe { mimi_list_set_product_to_string(list_ptr, arity) }
            } else {
                unsafe { mimi_list_set_product_to_json(list_ptr, arity) }
            };
            // SAFETY: `list_json` is a valid null-terminated C string returned by a Mimi allocation function
            let s = unsafe { cstr_to_string(list_json) };
            if !list_json.is_null() {
                mimi_free(list_json as *mut _);
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
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_result_list_option_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
        // SAFETY: `size` is a valid, non-negative allocation size
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            continue;
        }
        if bytes[i] != b'{' {
            // SAFETY: `pack` is a valid, non-null pointer from the `libc::malloc(16)` allocation at line 11961 (null-checked at 11962)
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
            // SAFETY: `pack` is a valid, non-null pointer from the `libc::malloc(16)` allocation at line 11961 (null-checked at 11962)
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
                // SAFETY: `c_arr` is non-null and points to memory allocated by `alloc_c_string` (null-checked at line 12050)
                mimi_free(c_arr as *mut _);
            }
            // SAFETY: `pack` is a valid, non-null pointer from the `libc::malloc(16)` allocation at line 11961 (null-checked at 11962)
            unsafe {
                *pack = 1;
                *pack.add(1) = list_ptr as i64;
            }
        }
        // SAFETY: `handle` is non-zero (validated at function entry line 11919) so `map_from_handle(handle)` returns a valid pointer
        let vh = pack as ValueHandle;
        // SAFETY: `map_from_handle(handle)` returns a valid, properly aligned pointer; `key` is a valid `String`
        unsafe {
            let mut map_ptr = map_from_handle(handle);
            (*map_ptr).inner.insert(key, vh);
            // §10-#35: this pack was malloc'd by the builder — register so
            // destroy() can reclaim its base (inner object handles are
            // intentional bounded leaks, see mimi_map_destroy comment).
            (*map_ptr)
                .owned
                .insert(vh, MapOwnedValueKind::PackErrCString);
        }
        MAP_OWNED_VALUE_BALANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    handle
}

/// Map of Result of List of Option of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_result_list_option_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        // SAFETY: `base` points to a valid, properly aligned value.
        if !pages_mapped(base as usize, 16) {
            parts.push(String::from("null"));
            continue;
        }
        let disc = unsafe { *base };
        if disc == 0 {
            // SAFETY: `err_ptr` is a valid null-terminated C string from a prior allocation
            let err_ptr = unsafe { *base.add(1) } as *const std::ffi::c_char;
            let err_s = if err_ptr.is_null() {
                String::new()
            } else {
                // SAFETY: `err_ptr` is a valid null-terminated C string returned by a Mimi allocation function
                unsafe { cstr_to_string(err_ptr) }
            };
            if display_style != 0 {
                parts.push(format!("Err({})", err_s));
            } else {
                parts.push(format!("{{\"Err\":[{}]}}", json_escape_string(&err_s)));
            }
        } else {
            // SAFETY: the offset is within bounds of `base`'s allocation; the handle value is valid for the target type
            let list_ptr = unsafe { *base.add(1) } as *const MimiList;
            let list_json =
                unsafe { mimi_list_option_product_to_json(list_ptr, arity, display_style) };
            // SAFETY: `list_json` is a valid null-terminated C string returned by a Mimi allocation function
            let s = unsafe { cstr_to_string(list_json) };
            if !list_json.is_null() {
                mimi_free(list_json as *mut _);
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
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_set_option_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
            // SAFETY: `c_arr` is non-null and points to memory allocated by `alloc_c_string` (null-checked at line 12223)
            mimi_free(c_arr as *mut _);
        }
        // SAFETY: `handle` is non-zero (validated at function entry line 12154) so `map_from_handle(handle)` returns a valid pointer
        unsafe {
            map_from_handle(handle)
                .inner
                .insert(key, set_h as ValueHandle);
        }
    }
    handle
}

/// Map of Set of Option of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_set_option_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        // SAFETY: `json_ptr` is a valid null-terminated C string returned by a Mimi allocation function
        let s = unsafe { cstr_to_string(json_ptr) };
        if !json_ptr.is_null() {
            mimi_free(json_ptr as *mut _);
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of Set of Result of Option of product from JSON.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_set_result_option_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
            // SAFETY: `c_arr` is non-null and points to memory allocated by `alloc_c_string` (null-checked at line 12367)
            mimi_free(c_arr as *mut _);
        }
        // SAFETY: `handle` is non-zero (validated at function entry line 12298) so `map_from_handle(handle)` returns a valid pointer
        unsafe {
            map_from_handle(handle)
                .inner
                .insert(key, set_h as ValueHandle);
        }
    }
    handle
}

/// Map of Set of Result of Option of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_set_result_option_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        // SAFETY: `json_ptr` is a valid null-terminated C string returned by a Mimi allocation function
        let s = unsafe { cstr_to_string(json_ptr) };
        if !json_ptr.is_null() {
            mimi_free(json_ptr as *mut _);
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of Set of Result of product from JSON.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_set_result_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
            // SAFETY: `c_arr` is non-null and points to memory allocated by `alloc_c_string` (null-checked at line 12511)
            mimi_free(c_arr as *mut _);
        }
        // SAFETY: `handle` is non-zero (validated at function entry line 12442) so `map_from_handle(handle)` returns a valid pointer
        unsafe {
            map_from_handle(handle)
                .inner
                .insert(key, set_h as ValueHandle);
        }
    }
    handle
}

/// Map of Set of Result of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_set_result_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        // SAFETY: `json_ptr` is a valid null-terminated C string returned by a Mimi allocation function
        let s = unsafe { cstr_to_string(json_ptr) };
        if !json_ptr.is_null() {
            mimi_free(json_ptr as *mut _);
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of List of Result of product from JSON.
/// Each map value is a list handle whose elements are Result product packs.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_list_result_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
            // SAFETY: `c_arr` is non-null and points to memory allocated by `alloc_c_string` (null-checked at line 12656)
            mimi_free(c_arr as *mut _);
        }
        // SAFETY: `handle` is non-zero (validated at function entry line 12587) so `map_from_handle(handle)` returns a valid pointer
        let vh = list_ptr as ValueHandle;
        // SAFETY: `map_from_handle(handle)` returns a valid, properly aligned pointer; `key` is a valid `String`
        unsafe {
            let mut map_ptr = map_from_handle(handle);
            (*map_ptr).inner.insert(key, vh);
            // §10-#35: this list_ptr was malloc'd by the builder — register so
            // destroy() can reclaim its base (inner object handles are
            // intentional bounded leaks, see mimi_map_destroy comment).
            (*map_ptr).owned.insert(vh, MapOwnedValueKind::ListObject);
        }
        MAP_OWNED_VALUE_BALANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    handle
}

/// Map of List of Result of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_list_result_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        let json_ptr = unsafe { mimi_list_result_product_to_json(list_ptr, arity, display_style) };
        // SAFETY: `json_ptr` is a valid null-terminated C string returned by a Mimi allocation function
        let s = unsafe { cstr_to_string(json_ptr) };
        if !json_ptr.is_null() {
            mimi_free(json_ptr as *mut _);
        }
        parts.push(s);
    }
    parts.push(String::from("}"));
    alloc_c_string(&parts.join(""))
}

/// Map of Option of List of product from JSON.
/// Pack: `{i64 disc, i64 list_handle}` disc 1=Some list, 0=None.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_option_list_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
        // SAFETY: `size` is a valid, non-negative allocation size
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            continue;
        }
        if bytes[i] == b'n' && i + 4 <= bytes.len() && &bytes[i..i + 4] == b"null" {
            i += 4;
            // SAFETY: `pack` is a valid, non-null pointer from the `libc::malloc(16)` allocation at line 12771 (null-checked at 12772)
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
                // SAFETY: `pack` is a valid, non-null pointer from the `libc::malloc(16)` allocation at line 12771 (null-checked at 12772)
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
                    // SAFETY: `size` is a valid, non-negative allocation size
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
            // SAFETY: `size` is a valid, non-negative allocation size
            let list_ptr = unsafe { libc::malloc(16) as *mut u8 };
            if list_ptr.is_null() {
                unsafe {
                    libc::free(pack as *mut _);
                }
                continue;
            }
            let data_size = prod_handles.len() * std::mem::size_of::<i64>();
            let data_ptr = if data_size > 0 {
                // SAFETY: `size` is a valid, non-negative allocation size
                unsafe { libc::malloc(data_size) as *mut i64 }
            } else {
                std::ptr::null_mut()
            };
            if !data_ptr.is_null() && !prod_handles.is_empty() {
                // SAFETY: Guarded by `!data_ptr.is_null() && !prod_handles.is_empty()` on line 12921; both pointers are valid and copy length is within bounds
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        prod_handles.as_ptr(),
                        data_ptr,
                        prod_handles.len(),
                    );
                }
            }
            // SAFETY: `list_ptr` is a valid non-null 16-byte allocation (line 12907, null-checked); `data_ptr` is null or valid allocation (line 12917); `pack` is valid (line 12771)
            unsafe {
                *(list_ptr as *mut i64) = prod_handles.len() as i64;
                *(list_ptr.add(8) as *mut *mut i64) = data_ptr;
                *pack = 1;
                *pack.add(1) = list_ptr as i64;
            }
        }
        // SAFETY: `handle` is non-zero (validated at function entry line 12728) so `map_from_handle(handle)` returns a valid pointer
        let vh = pack as ValueHandle;
        // SAFETY: `map_from_handle(handle)` returns a valid, properly aligned pointer; `key` is a valid `String`
        unsafe {
            let mut map_ptr = map_from_handle(handle);
            (*map_ptr).inner.insert(key, vh);
            // §10-#35: this pack was malloc'd by the builder — register so
            // destroy() can reclaim its base (inner object handles are
            // intentional bounded leaks, see mimi_map_destroy comment).
            (*map_ptr).owned.insert(vh, MapOwnedValueKind::Pack);
        }
        MAP_OWNED_VALUE_BALANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    handle
}

/// Map of Option of List of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_option_list_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        // SAFETY: `base` points to a valid, properly aligned value.
        if !pages_mapped(base as usize, 16) {
            parts.push(String::from("null"));
            continue;
        }
        let disc = unsafe { *base };
        if disc == 0 {
            if display_style != 0 {
                parts.push(String::from("None()"));
            } else {
                parts.push(String::from("\"None\""));
            }
        } else {
            // SAFETY: the offset is within bounds of `base`'s allocated buffer
            let list_ptr = unsafe { *base.add(1) } as *const u8;
            let tmp = mimi_map_new();
            if tmp != 0 && !list_ptr.is_null() {
                unsafe {
                    map_from_handle(tmp)
                        .inner
                        .insert("_".into(), list_ptr as ValueHandle);
                }
                let json_ptr = mimi_map_to_json_list_product_i64(tmp, arity, display_style);
                // SAFETY: `json_ptr` is a valid null-terminated C string returned by a Mimi allocation function
                let s = unsafe { cstr_to_string(json_ptr) };
                unsafe {
                    mimi_map_destroy(tmp);
                }
                if !json_ptr.is_null() {
                    mimi_free(json_ptr as *mut _);
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
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_result_list_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
        // SAFETY: `size` is a valid, non-negative allocation size
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            continue;
        }
        if bytes[i] != b'{' {
            // SAFETY: `pack` is a valid, non-null pointer from the `libc::malloc(16)` allocation at line 13086 (null-checked at 13087)
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
            // SAFETY: `pack` is a valid, non-null pointer from the `libc::malloc(16)` allocation at line 13086 (null-checked at 13087)
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
                    // SAFETY: `size` is a valid, non-negative allocation size
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
            // SAFETY: `size` is a valid, non-negative allocation size
            let list_ptr = unsafe { libc::malloc(16) as *mut u8 };
            if list_ptr.is_null() {
                unsafe {
                    libc::free(pack as *mut _);
                }
                continue;
            }
            let data_size = prod_handles.len() * std::mem::size_of::<i64>();
            let data_ptr = if data_size > 0 {
                // SAFETY: `size` is a valid, non-negative allocation size
                unsafe { libc::malloc(data_size) as *mut i64 }
            } else {
                std::ptr::null_mut()
            };
            if !data_ptr.is_null() && !prod_handles.is_empty() {
                // SAFETY: Guarded by `!data_ptr.is_null() && !prod_handles.is_empty()` on line 13245; both pointers are valid and copy length is within bounds
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        prod_handles.as_ptr(),
                        data_ptr,
                        prod_handles.len(),
                    );
                }
            }
            // SAFETY: `list_ptr` is a valid non-null 16-byte allocation (line 13231, null-checked); `data_ptr` is null or valid allocation (line 13241); `pack` is valid (line 13086)
            unsafe {
                *(list_ptr as *mut i64) = prod_handles.len() as i64;
                *(list_ptr.add(8) as *mut *mut i64) = data_ptr;
                *pack = 1;
                *pack.add(1) = list_ptr as i64;
            }
        }
        // SAFETY: `handle` is non-zero (validated at function entry line 13043) so `map_from_handle(handle)` returns a valid pointer
        let vh = pack as ValueHandle;
        // SAFETY: `map_from_handle(handle)` returns a valid, properly aligned pointer; `key` is a valid `String`
        unsafe {
            let mut map_ptr = map_from_handle(handle);
            (*map_ptr).inner.insert(key, vh);
            // §10-#35: this pack was malloc'd by the builder — register so
            // destroy() can reclaim its base (inner object handles are
            // intentional bounded leaks, see mimi_map_destroy comment).
            (*map_ptr)
                .owned
                .insert(vh, MapOwnedValueKind::PackErrCString);
        }
        MAP_OWNED_VALUE_BALANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    handle
}

/// Map of Result of List of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_result_list_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        // SAFETY: `base` points to a valid, properly aligned value.
        if !pages_mapped(base as usize, 16) {
            parts.push(String::from("null"));
            continue;
        }
        let disc = unsafe { *base };
        if disc == 0 {
            // SAFETY: `err_ptr` is a valid null-terminated C string from a prior allocation
            let err_ptr = unsafe { *base.add(1) } as *const std::ffi::c_char;
            let err_s = if err_ptr.is_null() {
                String::new()
            } else {
                // SAFETY: `err_ptr` is a valid null-terminated C string returned by a Mimi allocation function
                unsafe { cstr_to_string(err_ptr) }
            };
            if display_style != 0 {
                parts.push(format!("Err({})", err_s));
            } else {
                parts.push(format!("{{\"Err\":[{}]}}", json_escape_string(&err_s)));
            }
        } else {
            // SAFETY: the offset is within bounds of `base`'s allocated buffer
            let list_ptr = unsafe { *base.add(1) } as *const u8;
            // Format one list of product via temporary single-key map helper.
            let tmp = mimi_map_new();
            if tmp != 0 && !list_ptr.is_null() {
                unsafe {
                    map_from_handle(tmp)
                        .inner
                        .insert("_".into(), list_ptr as ValueHandle);
                }
                let json_ptr = mimi_map_to_json_list_product_i64(tmp, arity, display_style);
                // SAFETY: `json_ptr` is a valid null-terminated C string returned by a Mimi allocation function
                let s = unsafe { cstr_to_string(json_ptr) };
                unsafe {
                    mimi_map_destroy(tmp);
                }
                if !json_ptr.is_null() {
                    mimi_free(json_ptr as *mut _);
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
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_result_option_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
        // SAFETY: `size` is a valid, non-negative allocation size
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
                // SAFETY: `size` is a valid, non-negative allocation size
                let opt_ptr = unsafe { libc::malloc(opt_pack_size) as *mut i64 };
                if opt_ptr.is_null() {
                    unsafe {
                        libc::free(pack as *mut _);
                        if !c_opt.is_null() {
                            mimi_free(c_opt as *mut _);
                        }
                        if !c_arr.is_null() {
                            mimi_free(c_arr as *mut _);
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
                    // SAFETY: `opt_ptr` is a valid (n+1)-element i64 buffer from checked malloc at line 13464; `fields` has exactly n initialized elements; `pack` is a valid 2-element i64 buffer from checked malloc at line 13421
                }
                unsafe {
                    *opt_ptr = 1;
                    std::ptr::copy_nonoverlapping(fields.as_ptr(), opt_ptr.add(1), n);
                    *pack = 1;
                    *pack.add(1) = opt_ptr as i64;
                }
                // SAFETY: `c_opt` was allocated by alloc_c_string (non-null heap pointer); null guard ensures validity
                if !c_opt.is_null() {
                    mimi_free(c_opt as *mut _);
                }
                // SAFETY: `c_arr` was allocated by alloc_c_string (non-null heap pointer); null guard ensures validity
                if !c_arr.is_null() {
                    mimi_free(c_arr as *mut _);
                }
            // SAFETY: `pack` is a valid heap pointer from checked libc::malloc(16) at line 13421; no other reference exists
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
                // SAFETY: `pack` is a valid 2-element i64 buffer from checked malloc at line 13421; `c` is a valid C string pointer from alloc_c_string
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
                // SAFETY: `size` is a valid, non-negative allocation size
                let opt_ptr = unsafe { libc::malloc(opt_pack_size) as *mut i64 };
                if opt_ptr.is_null() {
                    unsafe {
                        libc::free(pack as *mut _);
                    }
                    break;
                }
                // SAFETY: `opt_ptr` is a valid (n+1)-element i64 buffer from checked malloc at line 13622; n <= 16 by function contract
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
                        // SAFETY: `opt_ptr` is a valid (n+1)-element i64 buffer from checked malloc; `fields` has n initialized elements; regions do not overlap
                    }
                    unsafe {
                        *opt_ptr = 1;
                        std::ptr::copy_nonoverlapping(fields.as_ptr(), opt_ptr.add(1), n);
                    }
                    // SAFETY: `pack` is a valid 2-element i64 buffer from checked malloc; opt_ptr cast to i64 is a valid value handle
                }
                unsafe {
                    *pack = 1;
                    *pack.add(1) = opt_ptr as i64;
                }
            }
            // SAFETY: `handle` was validated non-zero at line 13377; map_from_handle returns a valid, aligned map instance pointer
        }
        let vh = pack as ValueHandle;
        // SAFETY: `map_from_handle(handle)` returns a valid, properly aligned pointer; `key` is a valid `String`
        unsafe {
            let mut map_ptr = map_from_handle(handle);
            (*map_ptr).inner.insert(key, vh);
            // §10-#35: this pack was malloc'd by the builder — register so
            // destroy() can reclaim its base (inner object handles are
            // intentional bounded leaks, see mimi_map_destroy comment).
            (*map_ptr)
                .owned
                .insert(vh, MapOwnedValueKind::PackErrCString);
        }
        MAP_OWNED_VALUE_BALANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    handle
}

/// Map of Result of Option of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_result_option_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        // SAFETY: `base` points to a valid, properly aligned value.
        if !pages_mapped(base as usize, 16) {
            parts.push(String::from("null"));
            continue;
        }
        let disc = unsafe { *base };
        if disc == 0 {
            // SAFETY: `err_ptr` is a valid null-terminated C string from a prior allocation
            let err_ptr = unsafe { *base.add(1) } as *const std::ffi::c_char;
            let err_s = if err_ptr.is_null() {
                String::new()
            } else {
                // SAFETY: `err_ptr` is a valid null-terminated C string returned by a Mimi allocation function
                unsafe { cstr_to_string(err_ptr) }
            };
            if display_style != 0 {
                parts.push(format!("Err({})", err_s));
            } else {
                parts.push(format!("{{\"Err\":[{}]}}", json_escape_string(&err_s)));
            }
        } else {
            // SAFETY: the offset is within bounds of `base`'s allocated buffer
            let opt_h = unsafe { *base.add(1) } as *const i64;
            if opt_h.is_null() {
                if display_style != 0 {
                    parts.push(String::from("Ok(None())"));
                } else {
                    parts.push(String::from("{\"Ok\":[\"None\"]}"));
                }
            } else {
                // SAFETY: `opt_h` points to a valid, properly aligned value
                let opt_disc = unsafe { *opt_h };
                if opt_disc == 0 {
                    if display_style != 0 {
                        parts.push(String::from("Ok(None())"));
                    } else {
                        parts.push(String::from("{\"Ok\":[\"None\"]}"));
                    }
                } else {
                    // SAFETY: `opt_h` is a non-null pointer verified by discriminant check at line 13755; buffer has at least n+1 i64 elements; n <= 16 by function contract
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
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_result_set_map_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
        // SAFETY: `size` is a valid, non-negative allocation size
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            continue;
        }
        // SAFETY: `pack` was allocated by checked libc::malloc(16) at line 13834; no other reference exists
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
            // SAFETY: `pack` is a valid 2-element i64 buffer from checked malloc; `c` is a valid C string pointer from alloc_c_string
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
            // SAFETY: `c_arr` was allocated by alloc_c_string; null guard ensures validity
            if !c_arr.is_null() {
                mimi_free(c_arr as *mut _);
                // SAFETY: `pack` is a valid 2-element i64 buffer from checked malloc at line 13834
            }
            unsafe {
                *pack = 1;
                *pack.add(1) = ok_h as i64;
            }
            // SAFETY: `handle` was validated non-zero at line 13792; map_from_handle returns a valid, aligned map instance pointer
        }
        let vh = pack as ValueHandle;
        // SAFETY: `map_from_handle(handle)` returns a valid, properly aligned pointer; `key` is a valid `String`
        unsafe {
            let mut map_ptr = map_from_handle(handle);
            (*map_ptr).inner.insert(key, vh);
            // §10-#35: this pack was malloc'd by the builder — register so
            // destroy() can reclaim its base (inner object handles are
            // intentional bounded leaks, see mimi_map_destroy comment).
            (*map_ptr)
                .owned
                .insert(vh, MapOwnedValueKind::PackErrCString);
        }
        MAP_OWNED_VALUE_BALANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    handle
}

/// Map of Result of Set of Map of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_result_set_map_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        // SAFETY: `base` points to a valid, properly aligned value.
        if !pages_mapped(base as usize, 16) {
            parts.push(String::from("null"));
            continue;
        }
        let disc = unsafe { *base };
        if disc == 0 {
            // SAFETY: `err_ptr` is a valid null-terminated C string from a prior allocation
            let err_ptr = unsafe { *base.add(1) } as *const std::ffi::c_char;
            let err_s = if err_ptr.is_null() {
                String::new()
            } else {
                // SAFETY: `err_ptr` is a valid null-terminated C string returned by a Mimi allocation function
                unsafe { cstr_to_string(err_ptr) }
            };
            if display_style != 0 {
                parts.push(format!("Err({})", err_s));
            } else {
                parts.push(format!("{{\"Err\":[{}]}}", json_escape_string(&err_s)));
            }
        } else {
            // SAFETY: the offset is within bounds of `base`'s allocation; the handle value is valid for the target type
            let ok_h = unsafe { *base.add(1) };
            let ok_json = mimi_set_to_json_map_product_i64(ok_h as _, arity, display_style);
            // SAFETY: `ok_json` is a valid null-terminated C string returned by a Mimi allocation function
            let s = unsafe { cstr_to_string(ok_json) };
            if !ok_json.is_null() {
                mimi_free(ok_json as *mut _);
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
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_option_set_map_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
        // SAFETY: `size` is a valid, non-negative allocation size
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            continue;
        }
        if bytes[i] == b'n' && i + 4 <= bytes.len() && &bytes[i..i + 4] == b"null" {
            // SAFETY: `pack` is a valid 2-element i64 buffer from checked malloc at line 14099
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
            // SAFETY: `pack` was allocated by checked libc::malloc(16) at line 14099; no other reference exists
            } else {
                unsafe {
                    libc::free(pack as *mut _);
                }
                break;
            }
            let val = String::from_utf8_lossy(&bytes[val_start..i]).into_owned();
            let c_val = alloc_c_string(&val);
            let some_h = mimi_set_from_json_map_product_i64(c_val, arity);
            // SAFETY: `c_val` was allocated by alloc_c_string; null guard ensures validity
            if !c_val.is_null() {
                mimi_free(c_val as *mut _);
                // SAFETY: `pack` is a valid 2-element i64 buffer from checked malloc at line 14099
            }
            unsafe {
                *pack = 1;
                *pack.add(1) = some_h as i64;
            }
            // SAFETY: `handle` was validated non-zero at line 14057; map_from_handle returns a valid, aligned map instance pointer
        }
        let vh = pack as ValueHandle;
        // SAFETY: `map_from_handle(handle)` returns a valid, properly aligned pointer; `key` is a valid `String`
        unsafe {
            let mut map_ptr = map_from_handle(handle);
            (*map_ptr).inner.insert(key, vh);
            // §10-#35: this pack was malloc'd by the builder — register so
            // destroy() can reclaim its base (inner object handles are
            // intentional bounded leaks, see mimi_map_destroy comment).
            (*map_ptr).owned.insert(vh, MapOwnedValueKind::Pack);
        }
        MAP_OWNED_VALUE_BALANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    handle
}

/// Map of Option of Set of Map of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_option_set_map_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        // SAFETY: `base` points to a valid, properly aligned value.
        if !pages_mapped(base as usize, 16) {
            parts.push(String::from("null"));
            continue;
        }
        let disc = unsafe { *base };
        if disc == 0 {
            if display_style != 0 {
                parts.push(String::from("None()"));
            } else {
                parts.push(String::from("\"None\""));
            }
        } else {
            // SAFETY: the offset is within bounds of `base`'s allocation; the handle value is valid for the target type
            let some_h = unsafe { *base.add(1) };
            let some_json = mimi_set_to_json_map_product_i64(some_h as _, arity, display_style);
            // SAFETY: `some_json` is a valid null-terminated C string returned by a Mimi allocation function
            let s = unsafe { cstr_to_string(some_json) };
            if !some_json.is_null() {
                mimi_free(some_json as *mut _);
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
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_result_list_map_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
        // SAFETY: `size` is a valid, non-negative allocation size
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            continue;
        }
        // SAFETY: `pack` was allocated by checked libc::malloc(16) at line 14282; no other reference exists
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
            // SAFETY: `pack` is a valid 2-element i64 buffer from checked malloc at line 14282; `c` is a valid C string pointer from alloc_c_string
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
            // SAFETY: `c_arr` was allocated by alloc_c_string; null guard ensures validity
            if !c_arr.is_null() {
                mimi_free(c_arr as *mut _);
                // SAFETY: `pack` is a valid 2-element i64 buffer from checked malloc at line 14282
            }
            unsafe {
                *pack = 1;
                *pack.add(1) = ok_h as i64;
            }
            // SAFETY: `handle` was validated non-zero at line 14239; map_from_handle returns a valid, aligned map instance pointer
        }
        let vh = pack as ValueHandle;
        // SAFETY: `map_from_handle(handle)` returns a valid, properly aligned pointer; `key` is a valid `String`
        unsafe {
            let mut map_ptr = map_from_handle(handle);
            (*map_ptr).inner.insert(key, vh);
            // §10-#35: this pack was malloc'd by the builder — register so
            // destroy() can reclaim its base (inner object handles are
            // intentional bounded leaks, see mimi_map_destroy comment).
            (*map_ptr)
                .owned
                .insert(vh, MapOwnedValueKind::PackErrCString);
        }
        MAP_OWNED_VALUE_BALANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    handle
}

/// Map of Result of List of Map of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_result_list_map_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        // SAFETY: `base` points to a valid, properly aligned value.
        if !pages_mapped(base as usize, 16) {
            parts.push(String::from("null"));
            continue;
        }
        let disc = unsafe { *base };
        if disc == 0 {
            // SAFETY: `err_ptr` is a valid null-terminated C string from a prior allocation
            let err_ptr = unsafe { *base.add(1) } as *const std::ffi::c_char;
            let err_s = if err_ptr.is_null() {
                String::new()
            } else {
                // SAFETY: `err_ptr` is a valid null-terminated C string returned by a Mimi allocation function
                unsafe { cstr_to_string(err_ptr) }
            };
            if display_style != 0 {
                parts.push(format!("Err({})", err_s));
            } else {
                parts.push(format!("{{\"Err\":[{}]}}", json_escape_string(&err_s)));
            }
        } else {
            // SAFETY: the offset is within bounds of `base`'s allocation; the handle value is valid for the target type
            let ok_h = unsafe { *base.add(1) };
            let ok_json = unsafe { mimi_list_map_product_to_json(ok_h as _, arity, display_style) };
            // SAFETY: `ok_json` is a valid null-terminated C string returned by a Mimi allocation function
            let s = unsafe { cstr_to_string(ok_json) };
            if !ok_json.is_null() {
                mimi_free(ok_json as *mut _);
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
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_option_list_map_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
        // SAFETY: `size` is a valid, non-negative allocation size
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            continue;
        }
        if bytes[i] == b'n' && i + 4 <= bytes.len() && &bytes[i..i + 4] == b"null" {
            // SAFETY: `pack` is a valid 2-element i64 buffer from checked malloc at line 14547
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
            // SAFETY: `pack` was allocated by checked libc::malloc(16) at line 14547; no other reference exists
            } else {
                unsafe {
                    libc::free(pack as *mut _);
                }
                break;
            }
            let val = String::from_utf8_lossy(&bytes[val_start..i]).into_owned();
            let c_val = alloc_c_string(&val);
            let some_h = mimi_list_from_json_map_product_i64(c_val, arity);
            // SAFETY: `c_val` was allocated by alloc_c_string; null guard ensures validity
            if !c_val.is_null() {
                mimi_free(c_val as *mut _);
                // SAFETY: `pack` is a valid 2-element i64 buffer from checked malloc at line 14547
            }
            unsafe {
                *pack = 1;
                *pack.add(1) = some_h as i64;
            }
            // SAFETY: `handle` was validated non-zero at line 14504; map_from_handle returns a valid, aligned map instance pointer
        }
        let vh = pack as ValueHandle;
        // SAFETY: `map_from_handle(handle)` returns a valid, properly aligned pointer; `key` is a valid `String`
        unsafe {
            let mut map_ptr = map_from_handle(handle);
            (*map_ptr).inner.insert(key, vh);
            // §10-#35: this pack was malloc'd by the builder — register so
            // destroy() can reclaim its base (inner object handles are
            // intentional bounded leaks, see mimi_map_destroy comment).
            (*map_ptr).owned.insert(vh, MapOwnedValueKind::Pack);
        }
        MAP_OWNED_VALUE_BALANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    handle
}

/// Map of Option of List of Map of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_option_list_map_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        // SAFETY: `base` points to a valid, properly aligned value.
        if !pages_mapped(base as usize, 16) {
            parts.push(String::from("null"));
            continue;
        }
        let disc = unsafe { *base };
        if disc == 0 {
            if display_style != 0 {
                parts.push(String::from("None()"));
            } else {
                parts.push(String::from("\"None\""));
            }
        } else {
            // SAFETY: the offset is within bounds of `base`'s allocation; the handle value is valid for the target type
            let some_h = unsafe { *base.add(1) };
            let some_json =
                unsafe { mimi_list_map_product_to_json(some_h as _, arity, display_style) };
            // SAFETY: `some_json` is a valid null-terminated C string returned by a Mimi allocation function
            let s = unsafe { cstr_to_string(some_json) };
            if !some_json.is_null() {
                mimi_free(some_json as *mut _);
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
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_result_set_list_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
        // SAFETY: `size` is a valid, non-negative allocation size
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            continue;
        }
        // SAFETY: `pack` was allocated by checked libc::malloc(16) at line 14730; no other reference exists
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
            // SAFETY: `pack` is a valid 2-element i64 buffer from checked malloc at line 14730; `c` is a valid C string pointer from alloc_c_string
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
            // SAFETY: `c_arr` was allocated by alloc_c_string; null guard ensures validity
            if !c_arr.is_null() {
                mimi_free(c_arr as *mut _);
                // SAFETY: `pack` is a valid 2-element i64 buffer from checked malloc at line 14730
            }
            unsafe {
                *pack = 1;
                *pack.add(1) = set_h as i64;
            }
            // SAFETY: `handle` was validated non-zero at line 14688; map_from_handle returns a valid, aligned map instance pointer
        }
        let vh = pack as ValueHandle;
        // SAFETY: `map_from_handle(handle)` returns a valid, properly aligned pointer; `key` is a valid `String`
        unsafe {
            let mut map_ptr = map_from_handle(handle);
            (*map_ptr).inner.insert(key, vh);
            // §10-#35: this pack was malloc'd by the builder — register so
            // destroy() can reclaim its base (inner object handles are
            // intentional bounded leaks, see mimi_map_destroy comment).
            (*map_ptr)
                .owned
                .insert(vh, MapOwnedValueKind::PackErrCString);
        }
        MAP_OWNED_VALUE_BALANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    handle
}

/// Map of Result of Set of List of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_result_set_list_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        // SAFETY: `base` points to a valid, properly aligned value.
        if !pages_mapped(base as usize, 16) {
            parts.push(String::from("null"));
            continue;
        }
        let disc = unsafe { *base };
        if disc == 0 {
            // SAFETY: `err_ptr` is a valid null-terminated C string from a prior allocation
            let err_ptr = unsafe { *base.add(1) } as *const std::ffi::c_char;
            let err_s = if err_ptr.is_null() {
                String::new()
            } else {
                // SAFETY: `err_ptr` is a valid null-terminated C string returned by a Mimi allocation function
                unsafe { cstr_to_string(err_ptr) }
            };
            if display_style != 0 {
                parts.push(format!("Err({})", err_s));
            } else {
                parts.push(format!("{{\"Err\":[{}]}}", json_escape_string(&err_s)));
            }
        } else {
            // SAFETY: the offset is within bounds of `base`'s allocation; the handle value is valid for the target type
            let set_h = unsafe { *base.add(1) } as SetHandle;
            let set_json = mimi_set_to_json_list_product_i64(set_h, arity, display_style);
            // SAFETY: `set_json` is a valid null-terminated C string returned by a Mimi allocation function
            let s = unsafe { cstr_to_string(set_json) };
            if !set_json.is_null() {
                mimi_free(set_json as *mut _);
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
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_result_set_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
        // SAFETY: `size` is a valid, non-negative allocation size
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            continue;
        }
        // SAFETY: `pack` was allocated by checked libc::malloc(16) at line 14966; no other reference exists
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
            // SAFETY: `pack` is a valid 2-element i64 buffer from checked malloc at line 14966; `c` is a valid C string pointer from alloc_c_string
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
            // SAFETY: `c_arr` was allocated by alloc_c_string; null guard ensures validity
            if !c_arr.is_null() {
                mimi_free(c_arr as *mut _);
                // SAFETY: `pack` is a valid 2-element i64 buffer from checked malloc at line 14966
            }
            unsafe {
                *pack = 1;
                *pack.add(1) = set_h as i64;
            }
            // SAFETY: `handle` was validated non-zero at line 14924; map_from_handle returns a valid, aligned map instance pointer
        }
        let vh = pack as ValueHandle;
        // SAFETY: `map_from_handle(handle)` returns a valid, properly aligned pointer; `key` is a valid `String`
        unsafe {
            let mut map_ptr = map_from_handle(handle);
            (*map_ptr).inner.insert(key, vh);
            // §10-#35: this pack was malloc'd by the builder — register so
            // destroy() can reclaim its base (inner object handles are
            // intentional bounded leaks, see mimi_map_destroy comment).
            (*map_ptr)
                .owned
                .insert(vh, MapOwnedValueKind::PackErrCString);
        }
        MAP_OWNED_VALUE_BALANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    handle
}

/// Map of Result of Set of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_result_set_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        // SAFETY: `base` points to a valid, properly aligned value.
        if !pages_mapped(base as usize, 16) {
            parts.push(String::from("null"));
            continue;
        }
        let disc = unsafe { *base };
        if disc == 0 {
            // SAFETY: `err_ptr` is a valid null-terminated C string from a prior allocation
            let err_ptr = unsafe { *base.add(1) } as *const std::ffi::c_char;
            let err_s = if err_ptr.is_null() {
                String::new()
            } else {
                // SAFETY: `err_ptr` is a valid null-terminated C string returned by a Mimi allocation function
                unsafe { cstr_to_string(err_ptr) }
            };
            if display_style != 0 {
                parts.push(format!("Err({})", err_s));
            } else {
                parts.push(format!("{{\"Err\":[{}]}}", json_escape_string(&err_s)));
            }
        } else {
            // SAFETY: the offset is within bounds of `base`'s allocation; the handle value is valid for the target type
            let set_h = unsafe { *base.add(1) } as SetHandle;
            let set_json = mimi_set_to_json_product_i64(set_h, arity, display_style);
            // SAFETY: `set_json` is a valid null-terminated C string returned by a Mimi allocation function
            let s = unsafe { cstr_to_string(set_json) };
            if !set_json.is_null() {
                mimi_free(set_json as *mut _);
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
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_option_set_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
        // SAFETY: `size` is a valid, non-negative allocation size
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            continue;
        }
        // SAFETY: `pack` is a valid 2-element i64 buffer from checked malloc at line 15212
        if is_none {
            unsafe {
                *pack = 0;
                *pack.add(1) = 0;
            }
        } else {
            // SAFETY: `pack` was allocated by checked libc::malloc(16) at line 15212; no other reference exists
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
            // SAFETY: `c_arr` was allocated by alloc_c_string; null guard ensures validity
            if !c_arr.is_null() {
                // SAFETY: `c_arr` is non-null (checked above) and was allocated by `alloc_c_string` which uses `libc::malloc`
                mimi_free(c_arr as *mut _);
                // SAFETY: `pack` is a valid 2-element i64 buffer from checked malloc at line 15212
            }
            // SAFETY: `pack` was allocated by `libc::malloc(16)` at line 15212 and confirmed non-null
            unsafe {
                *pack = 1;
                *pack.add(1) = set_h as i64;
            }
            // SAFETY: `handle` was validated non-zero at line 15160; map_from_handle returns a valid, aligned map instance pointer
        }
        // SAFETY: `handle` is a valid `MapHandle` from `mimi_map_new()`; `map_from_handle` aborts on invalid handles
        let vh = pack as ValueHandle;
        // SAFETY: `map_from_handle(handle)` returns a valid, properly aligned pointer; `key` is a valid `String`
        unsafe {
            let mut map_ptr = map_from_handle(handle);
            (*map_ptr).inner.insert(key, vh);
            // §10-#35: this pack was malloc'd by the builder — register so
            // destroy() can reclaim its base (inner object handles are
            // intentional bounded leaks, see mimi_map_destroy comment).
            (*map_ptr).owned.insert(vh, MapOwnedValueKind::Pack);
        }
        MAP_OWNED_VALUE_BALANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    handle
}

/// Map of Option of Set of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_option_set_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        // SAFETY: `base` points to a valid, properly aligned value.
        if !pages_mapped(base as usize, 16) {
            parts.push(String::from("null"));
            continue;
        }
        let disc = unsafe { *base };
        if disc == 0 {
            if display_style != 0 {
                parts.push(String::from("None()"));
            } else {
                parts.push(String::from("\"None\""));
            }
        } else {
            // SAFETY: the offset is within bounds of `base`'s allocation; the handle value is valid for the target type
            let set_h = unsafe { *base.add(1) } as SetHandle;
            let set_json = mimi_set_to_json_product_i64(set_h, arity, display_style);
            // SAFETY: `set_json` is a valid null-terminated C string returned by a Mimi allocation function
            let s = unsafe { cstr_to_string(set_json) };
            if !set_json.is_null() {
                mimi_free(set_json as *mut _);
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
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_option_map_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
        // SAFETY: `size` is a valid, non-negative allocation size
        let pack = unsafe { libc::malloc(16) as *mut i64 };
        if pack.is_null() {
            continue;
        }
        if is_none {
            // SAFETY: `pack` was allocated by `libc::malloc(16)` at line 15404 and confirmed non-null
            unsafe {
                *pack = 0;
                *pack.add(1) = 0;
            }
        } else {
            // Extract object value as substring for nested map_from_json_product.
            if bytes[i] != b'{' {
                // SAFETY: `pack` was allocated by `libc::malloc(16)` at line 15404 and confirmed non-null at line 15405
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
                // SAFETY: `c_obj` is non-null (checked above) and was allocated by `alloc_c_string` which uses `libc::malloc`
                mimi_free(c_obj as *mut _);
            }
            // SAFETY: `pack` was allocated by `libc::malloc(16)` at line 15404 and confirmed non-null
            unsafe {
                *pack = 1;
                *pack.add(1) = inner_h as i64;
            }
        }
        // SAFETY: `handle` is a valid `MapHandle` from `mimi_map_new()`; `map_from_handle` aborts on invalid handles
        let vh = pack as ValueHandle;
        // SAFETY: `map_from_handle(handle)` returns a valid, properly aligned pointer; `key` is a valid `String`
        unsafe {
            let mut map_ptr = map_from_handle(handle);
            (*map_ptr).inner.insert(key, vh);
            // §10-#35: this pack was malloc'd by the builder — register so
            // destroy() can reclaim its base (inner object handles are
            // intentional bounded leaks, see mimi_map_destroy comment).
            (*map_ptr).owned.insert(vh, MapOwnedValueKind::Pack);
        }
        MAP_OWNED_VALUE_BALANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    handle
}

/// Map of Option of Map of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_option_map_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        // SAFETY: `base` points to a valid, properly aligned value.
        if !pages_mapped(base as usize, 16) {
            parts.push(String::from("null"));
            continue;
        }
        let disc = unsafe { *base };
        if disc == 0 {
            if display_style != 0 {
                parts.push(String::from("None()"));
            } else {
                parts.push(String::from("\"None\""));
            }
        } else {
            // SAFETY: the offset is within bounds of `base`'s allocation; the handle value is valid for the target type
            let inner_h = unsafe { *base.add(1) } as MapHandle;
            let inner_json = mimi_map_to_json_product_i64(inner_h, arity, display_style);
            // SAFETY: `inner_json` is a valid null-terminated C string returned by a Mimi allocation function
            let s = unsafe { cstr_to_string(inner_json) };
            if !inner_json.is_null() {
                mimi_free(inner_json as *mut _);
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
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_option_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
                                   // SAFETY: `size` is a valid, non-negative allocation size
        let ptr = unsafe { libc::malloc(pack_size) as *mut i64 };
        if ptr.is_null() {
            continue;
        }
        if is_none {
            // SAFETY: `ptr` was allocated by `libc::malloc(pack_size)` at line 15601 and confirmed non-null at line 15602
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
                // SAFETY: `ptr` was allocated by `libc::malloc(pack_size)` at line 15601 and confirmed non-null at line 15602
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
            // SAFETY: `ptr` was allocated by `libc::malloc(pack_size)` with space for `n` fields (confirmed non-null); `fields.as_ptr()` is a valid Rust slice of length `n`
            unsafe {
                *ptr = 1;
                std::ptr::copy_nonoverlapping(fields.as_ptr(), ptr.add(1), n);
            }
        }
        let vh = ptr as ValueHandle;
        // SAFETY: `handle` is a valid `MapHandle` from `mimi_map_new()`; `map_from_handle` aborts on invalid handles
        unsafe {
            let mut map_ptr = map_from_handle(handle);
            (*map_ptr).inner.insert(key, vh);
            // §10-#35: register the malloc'd option pack so destroy() reclaims it.
            (*map_ptr).owned.insert(vh, MapOwnedValueKind::Pack);
        }
        MAP_OWNED_VALUE_BALANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    handle
}

/// Map of Option of product Display/JSON.
/// `display_style` 0 = JSON, 1 = Display.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_option_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        // SAFETY: `base` points to a valid, properly aligned value.
        if !pages_mapped(base as usize, 16) {
            parts.push(String::from("null"));
            continue;
        }
        let disc = unsafe { *base };
        if disc == 0 {
            if display_style != 0 {
                parts.push(String::from("None()"));
            } else {
                parts.push(String::from("\"None\""));
            }
        } else {
            // SAFETY: `base` points to a heap allocation of at least `8 + n*8` bytes (pack_size from line 15601); disc=1 indicates valid Some payload
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
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_result_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> MapHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_map_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
        // SAFETY: `size` is a valid, non-negative allocation size
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
            // SAFETY: `ptr` was allocated by `libc::malloc(pack_size)` at line 15806 and confirmed non-null; `c` is a valid C string ptr from `alloc_c_string`
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
                // SAFETY: `ptr` was allocated by `libc::malloc(pack_size)` at line 15806 and confirmed non-null at line 15807
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
            // SAFETY: `ptr` was allocated by `libc::malloc(pack_size)` with space for `n` fields (confirmed non-null); `fields.as_ptr()` is a valid Rust slice of length `n`
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
        // SAFETY: `handle` is a valid `MapHandle` from `mimi_map_new()`; `map_from_handle` aborts on invalid handles
        unsafe {
            let mut map_ptr = map_from_handle(handle);
            (*map_ptr).inner.insert(key, vh);
            // §10-#35: register the malloc'd result pack so destroy() reclaims it.
            // Residual: an Err pack's embedded C string (ptr[1]) is not
            // separately reclaimed — documented known boundary (LOW item).
            (*map_ptr).owned.insert(vh, MapOwnedValueKind::Pack);
        }
        MAP_OWNED_VALUE_BALANCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    handle
}

/// Map of Result of product Display/JSON.
#[no_mangle]
pub unsafe extern "C" fn mimi_map_to_json_result_product_i64(
    handle: MapHandle,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if handle == 0 || arity <= 0 || arity > 16 {
        return alloc_c_string("{}");
    }
    // SAFETY: `map_from_handle` is non-null and points to a valid map instance
    let map = map_from_handle(handle);
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
        // SAFETY: `base` points to a valid, properly aligned value.
        if !pages_mapped(base as usize, 16) {
            parts.push(String::from("null"));
            continue;
        }
        let disc = unsafe { *base };
        if disc == 0 {
            // SAFETY: `err_ptr` is a valid null-terminated C string from a prior allocation
            let err_ptr = unsafe { *base.add(1) } as *const std::ffi::c_char;
            let err_s = if err_ptr.is_null() {
                String::new()
            } else {
                // SAFETY: `err_ptr` is a valid null-terminated C string returned by a Mimi allocation function
                unsafe { cstr_to_string(err_ptr) }
            };
            if display_style != 0 {
                parts.push(format!("Err({})", err_s));
            } else {
                parts.push(format!("{{\"Err\":[{}]}}", json_escape_string(&err_s)));
            }
        } else {
            // SAFETY: `base` points to a heap allocation of at least `8 + n*8` bytes (pack_size from line 15806); disc=1 indicates valid Ok payload
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
///
/// # Safety
/// JSON string pointers must be valid NUL-terminated C strings
/// (or null where documented).
#[no_mangle]
pub unsafe extern "C" fn mimi_map_from_json_i64(json: *const std::ffi::c_char) -> MapHandle {
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
            map_from_handle(handle)
                .inner
                .insert(key, v_i64 as ValueHandle);
        }
        count += 1;
    }
    handle
}

///
/// # Safety
/// JSON string pointers must be valid NUL-terminated C strings
/// (or null where documented).
#[no_mangle]
pub unsafe extern "C" fn mimi_json_as_i64(json: *const std::ffi::c_char) -> i64 {
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

///
/// # Safety
/// JSON string pointers must be valid NUL-terminated C strings
/// (or null where documented).
#[no_mangle]
pub unsafe extern "C" fn mimi_json_as_f64(json: *const std::ffi::c_char) -> f64 {
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

///
/// # Safety
/// JSON string pointers must be valid NUL-terminated C strings
/// (or null where documented).
#[no_mangle]
pub unsafe extern "C" fn mimi_json_as_bool(json: *const std::ffi::c_char) -> i64 {
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

pub(super) struct MimiSet {
    pub(super) inner: std::collections::HashSet<SetValueHandle>,
}

/// S4: Return raw pointer instead of &'static mut to avoid aliasing UB.
/// S18: abort() instead of panic! — panic across FFI boundary is UB (Rust ABI requirement).
/// R-C11: also aborts on stale (destroyed / never-registered) handles.
/// batch4-05 P1-2: like map handles, set handles must not be destroyed by
/// another thread while an operation is using them; no lease mechanism yet.
// SAFETY: aborts on invalid/stale handle; caller must ensure exclusive access while live.
fn set_from_handle(handle: SetHandle) -> handle::SetLease {
    match handle::set_acquire(handle) {
        Ok(lease) => lease,
        Err(e) => {
            handle::set_handle_error(e);
            std::process::abort();
        }
    }
}

#[no_mangle]
pub extern "C" fn mimi_set_new() -> SetHandle {
    handle::set_new_handle(MimiSet {
        inner: std::collections::HashSet::new(),
    })
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
pub unsafe extern "C" fn mimi_set_to_display(handle: SetHandle) -> *mut std::ffi::c_char {
    set_to_display_impl(handle, false)
}

/// Display form `Set{true, false}` for bool-valued sets.
#[no_mangle]
pub unsafe extern "C" fn mimi_set_to_display_bool(handle: SetHandle) -> *mut std::ffi::c_char {
    set_to_display_impl(handle, true)
}

fn set_to_display_impl(handle: SetHandle, as_bool: bool) -> *mut std::ffi::c_char {
    if handle == 0 {
        return alloc_c_string("Set{}");
    }
    // SAFETY: non-zero SetHandle.
    let set = set_from_handle(handle);
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
pub unsafe extern "C" fn mimi_set_to_json_i64(handle: SetHandle) -> *mut std::ffi::c_char {
    if handle == 0 {
        return alloc_c_string("[]");
    }
    // SAFETY: non-zero SetHandle from mimi_set_new / from_json.
    let set = set_from_handle(handle);
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
pub unsafe extern "C" fn mimi_set_to_json_product_i64(
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
    // SAFETY: `set_from_handle` is non-null and points to a valid `MimiSet`
    let set = set_from_handle(handle);
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
                // §10-#31: mincore-probed read (bare from_raw_parts segfaulted
                // on corrupt product handles).
                safe_read_product_fields(*vh as ValueHandle, n).unwrap_or_else(|| vec![0; n])
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
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_from_json_result_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> *mut MimiList {
    // audit-wave1 CRITICAL (allocator mismatch): Box allocation + explicit
    // element_kind (elements are malloc'd `{disc, ok/err}` packs → Record).
    let empty = || Box::into_raw(Box::new(MimiList::new_with_kind(ListElementKind::Record)));
    if json.is_null() || arity <= 0 || arity > 16 {
        return empty();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
        // SAFETY: `size` is a valid, non-negative allocation size
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
            // SAFETY: `size` is a valid, non-negative allocation size
            let heap = unsafe { libc::malloc(16) as *mut i64 };
            if !heap.is_null() {
                unsafe {
                    *heap = c as i64;
                    *heap.add(1) = err_str.len() as i64;
                }
            }
            // SAFETY: `ptr` was allocated by `libc::malloc(pack_size)` at line 16400 and confirmed non-null at line 16401; `c` is a valid C string ptr from `alloc_c_string`
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
                // SAFETY: `ptr` was allocated by `libc::malloc(pack_size)` at line 16400 and confirmed non-null at line 16401
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
            // SAFETY: `ptr` was allocated by `libc::malloc(pack_size)` with space for `n` Ok fields + err word; `fields.as_ptr()` is a valid Rust slice of length `n`
            unsafe {
                *ptr = 1;
                std::ptr::copy_nonoverlapping(fields.as_ptr(), ptr.add(1), n);
                *ptr.add(1 + n) = 0;
            }
        }
        handles.push(ptr as i64);
    }
    let data_size = handles.len() * 8;
    let data = if data_size == 0 {
        std::ptr::null_mut()
    } else {
        // SAFETY: `data_size` is positive; result is checked for null below.
        unsafe { libc::malloc(data_size) as *mut i64 }
    };
    if data_size > 0 && data.is_null() {
        return std::ptr::null_mut();
    }
    if !data.is_null() {
        // SAFETY: `data` is non-null (checked above) and points to `handles.len() * 8` bytes from `libc::malloc`; `handles` is a valid Vec with matching length
        unsafe {
            std::ptr::copy_nonoverlapping(handles.as_ptr(), data, handles.len());
        }
    }
    // audit-wave1 CRITICAL: Box-allocated struct (mimi_list_free frees via
    // Box::from_raw) + explicit element_kind (malloc'd result packs → Record).
    Box::into_raw(Box::new(MimiList::with_data(
        data as *mut *mut std::ffi::c_char,
        handles.len() as i64,
        true,
        ListElementKind::Record,
    )))
}

/// List of Result of Set of product from JSON.
/// Elements: heap `{i64 disc, i64 set_or_err}`.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_from_json_result_set_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> *mut MimiList {
    // audit-wave1 CRITICAL (allocator mismatch): Box allocation + explicit
    // element_kind (elements are malloc'd 24-byte `{disc, ok, set_or_err}`
    // packs → Record).
    let empty = || Box::into_raw(Box::new(MimiList::new_with_kind(ListElementKind::Record)));
    if json.is_null() || arity <= 0 || arity > 16 {
        return empty();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
        // SAFETY: `size` is a valid, non-negative allocation size
        // disc + Ok SetHandle + Err (heap Mimi string)
        let pack = unsafe { libc::malloc(24) as *mut i64 };
        if pack.is_null() {
            break;
        }
        // SAFETY: `pack` was allocated by `libc::malloc(24)` at line 16639 and confirmed non-null at line 16640
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
            // SAFETY: `size` is a valid, non-negative allocation size
            let heap = unsafe { libc::malloc(16) as *mut i64 };
            if !heap.is_null() {
                unsafe {
                    *heap = c as i64;
                    *heap.add(1) = err_s.len() as i64;
                }
            }
            // SAFETY: `pack` was allocated by `libc::malloc(24)` at line 16639 and confirmed non-null; `c` is a valid C string ptr from `alloc_c_string`
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
                // SAFETY: `c_arr` is non-null (checked above) and was allocated by `alloc_c_string` which uses `libc::malloc`
                mimi_free(c_arr as *mut _);
            }
            // SAFETY: `pack` was allocated by `libc::malloc(24)` at line 16639 and confirmed non-null at line 16640
            unsafe {
                *pack = 1;
                *pack.add(1) = set_h as i64;
                *pack.add(2) = 0;
            }
        }
        handles.push(pack as i64);
    }
    let data_size = handles.len() * 8;
    let data = if data_size == 0 {
        std::ptr::null_mut()
    } else {
        // SAFETY: `data_size` is positive; result is checked for null below.
        unsafe { libc::malloc(data_size) as *mut i64 }
    };
    if data_size > 0 && data.is_null() {
        return std::ptr::null_mut();
    }
    if !data.is_null() {
        // SAFETY: `data` is non-null (checked above) and points to `handles.len() * 8` bytes from `libc::malloc`; `handles` is a valid Vec with matching length
        unsafe {
            std::ptr::copy_nonoverlapping(handles.as_ptr(), data, handles.len());
        }
    }
    // audit-wave1 CRITICAL: Box-allocated struct (mimi_list_free frees via
    // Box::from_raw) + explicit element_kind (malloc'd packs → Record).
    Box::into_raw(Box::new(MimiList::with_data(
        data as *mut *mut std::ffi::c_char,
        handles.len() as i64,
        true,
        ListElementKind::Record,
    )))
}

/// Display/JSON for List of Result of Set of product.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_result_set_product_to_json(
    list: *const MimiList,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if list.is_null() || arity <= 0 || arity > 16 {
        return alloc_c_string("[]");
    }
    // SAFETY: `list` is non-null and points to a valid `MimiList`
    let lst = unsafe { &*list.cast::<MimiListAbiPrefix>() };
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
        // SAFETY: `lst.data` points to a valid, properly aligned value
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
        // SAFETY: `base` points to a valid, properly aligned value.
        if !pages_mapped(base as usize, 24) {
            parts.push(String::from("null"));
            continue;
        }
        let disc_word = unsafe { *base };
        let is_ok = (disc_word & 1) != 0;
        if !is_ok {
            // SAFETY: the offset is within bounds of `base`'s allocated buffer
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
            // SAFETY: the offset is within bounds of `base`'s allocation; the handle value is valid for the target type
            let set_h = unsafe { *base.add(1) } as SetHandle;
            let sj = mimi_set_to_json_product_i64(set_h, arity, display_style);
            // SAFETY: `sj` is a valid null-terminated C string returned by a Mimi allocation function
            let s = unsafe { cstr_to_string(sj) };
            if !sj.is_null() {
                mimi_free(sj as *mut _);
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
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_from_json_result_map_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> *mut MimiList {
    // audit-wave1 CRITICAL (allocator mismatch): Box allocation + explicit
    // element_kind (elements are malloc'd 24-byte `{disc, ok, map_or_err}`
    // packs → Record).
    let empty = || Box::into_raw(Box::new(MimiList::new_with_kind(ListElementKind::Record)));
    if json.is_null() || arity <= 0 || arity > 16 {
        return empty();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
        // SAFETY: `size` is a valid, non-negative allocation size
        // disc + Ok MapHandle + Err (heap Mimi string)
        let pack = unsafe { libc::malloc(24) as *mut i64 };
        if pack.is_null() {
            break;
        }
        // SAFETY: `pack` was allocated by `libc::malloc(24)` at line 16909 and confirmed non-null at line 16910
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
            // SAFETY: `size` is a valid, non-negative allocation size
            let heap = unsafe { libc::malloc(16) as *mut i64 };
            if !heap.is_null() {
                unsafe {
                    *heap = c as i64;
                    *heap.add(1) = err_s.len() as i64;
                }
            }
            // SAFETY: `pack` was allocated by `libc::malloc(24)` at line 16909 and confirmed non-null; `c` is a valid C string ptr from `alloc_c_string`
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
                // SAFETY: `c_obj` is non-null (checked above) and was allocated by `alloc_c_string` which uses `libc::malloc`
                mimi_free(c_obj as *mut _);
            }
            // SAFETY: `pack` was allocated by `libc::malloc(24)` at line 16909 and confirmed non-null at line 16910
            unsafe {
                *pack = 1;
                *pack.add(1) = mh as i64;
                *pack.add(2) = 0;
            }
        }
        handles.push(pack as i64);
    }
    let data_size = handles.len() * 8;
    let data = if data_size == 0 {
        std::ptr::null_mut()
    } else {
        // SAFETY: `data_size` is positive; result is checked for null below.
        unsafe { libc::malloc(data_size) as *mut i64 }
    };
    if data_size > 0 && data.is_null() {
        return std::ptr::null_mut();
    }
    if !data.is_null() {
        // SAFETY: `data` is non-null (checked above) and points to `handles.len() * 8` bytes from `libc::malloc`; `handles` is a valid Vec with matching length
        unsafe {
            std::ptr::copy_nonoverlapping(handles.as_ptr(), data, handles.len());
        }
    }
    // audit-wave1 CRITICAL: Box-allocated struct (mimi_list_free frees via
    // Box::from_raw) + explicit element_kind (malloc'd packs → Record).
    Box::into_raw(Box::new(MimiList::with_data(
        data as *mut *mut std::ffi::c_char,
        handles.len() as i64,
        true,
        ListElementKind::Record,
    )))
}

/// Display/JSON for List of Result of Map of product.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_result_map_product_to_json(
    list: *const MimiList,
    arity: i64,
    display_style: i64,
) -> *mut std::ffi::c_char {
    if list.is_null() || arity <= 0 || arity > 16 {
        return alloc_c_string("[]");
    }
    // SAFETY: `list` is non-null and points to a valid `MimiList`
    let lst = unsafe { &*list.cast::<MimiListAbiPrefix>() };
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
        // SAFETY: `lst.data` points to a valid, properly aligned value
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
        // SAFETY: `base` points to a valid, properly aligned value.
        if !pages_mapped(base as usize, 24) {
            parts.push(String::from("null"));
            continue;
        }
        let disc_word = unsafe { *base };
        let is_ok = (disc_word & 1) != 0;
        if !is_ok {
            // SAFETY: the offset is within bounds of `base`'s allocated buffer
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
            // SAFETY: the offset is within bounds of `base`'s allocation; the handle value is valid for the target type
            let mh = unsafe { *base.add(1) } as MapHandle;
            let mj = mimi_map_to_json_product_i64(mh, arity, display_style);
            // SAFETY: `mj` is a valid null-terminated C string returned by a Mimi allocation function
            let s = unsafe { cstr_to_string(mj) };
            if !mj.is_null() {
                mimi_free(mj as *mut _);
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
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_result_product_to_json(
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
    // SAFETY: `list` is non-null and points to a valid `MimiList`
    let lst = unsafe { &*list.cast::<MimiListAbiPrefix>() };
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
        // SAFETY: `lst.data` points to a valid, properly aligned value
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
        // SAFETY: `base` points to a valid, properly aligned value.
        if !pages_mapped(base as usize, 8 * (n + 2)) {
            parts.push(String::from("null"));
            continue;
        }
        let disc_word = unsafe { *base };
        let is_ok = (disc_word & 1) != 0;
        if !is_ok {
            // SAFETY: the offset is within bounds of `base`'s allocated buffer
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
            // SAFETY: the pointer is non-null and the buffer has at least `n` elements; the data is valid for the read lifetime
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
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_set_from_json_result_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> SetHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_set_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
        // SAFETY: `size` is a valid, non-negative allocation size
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
            // SAFETY: `ptr` was allocated by `libc::malloc(pack_size)` at line 17225 and confirmed non-null; `c` is a valid C string ptr from `alloc_c_string`
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
                // SAFETY: `ptr` was allocated by `libc::malloc(pack_size)` at line 17225 and confirmed non-null at line 17226
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
            // SAFETY: `ptr` was allocated by `libc::malloc(pack_size)` with space for `n` fields (confirmed non-null); `fields.as_ptr()` is a valid Rust slice of length `n`
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
pub unsafe extern "C" fn mimi_set_to_json_result_product_i64(
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
    // SAFETY: `set_from_handle` is non-null and points to a valid `MimiSet`
    let set = set_from_handle(handle);
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
                    // SAFETY: `ptr` points to a valid, properly aligned value
                    let disc = unsafe { *ptr };
                    if disc == 0 {
                        // SAFETY: `err_ptr` is a valid null-terminated C string from a prior allocation
                        let err_ptr = unsafe { *ptr.add(1) } as *const std::ffi::c_char;
                        let err_s = if err_ptr.is_null() {
                            String::new()
                        } else {
                            // SAFETY: the pointer is non-null and points to at least `n` initialized elements
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
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_set_from_json_option_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> SetHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_set_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
        // SAFETY: `size` is a valid, non-negative allocation size
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
            // SAFETY: `ptr` was allocated by `libc::malloc(pack_size)` at line 17475 and confirmed non-null at line 17476
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
                // SAFETY: `ptr` was allocated by `libc::malloc(pack_size)` at line 17475 and confirmed non-null at line 17476
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
            // SAFETY: `ptr` was allocated by `libc::malloc(pack_size)` with space for `n` fields (confirmed non-null); `fields.as_ptr()` is a valid Rust slice of length `n`
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
pub unsafe extern "C" fn mimi_set_to_json_option_product_i64(
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
    // SAFETY: `set_from_handle` is non-null and points to a valid `MimiSet`
    let set = set_from_handle(handle);
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
                    // SAFETY: `ptr` points to a valid, properly aligned value
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
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_set_from_json_product_i64(
    json: *const std::ffi::c_char,
    arity: i64,
) -> SetHandle {
    if json.is_null() || arity <= 0 || arity > 16 {
        return mimi_set_new();
    }
    // SAFETY: `json` is a valid null-terminated C string returned by a Mimi allocation function
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
        // SAFETY: `size` is a valid, non-negative allocation size
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
pub unsafe extern "C" fn mimi_set_to_json_bool(handle: SetHandle) -> *mut std::ffi::c_char {
    if handle == 0 {
        return alloc_c_string("[]");
    }
    // SAFETY: non-zero SetHandle.
    let set = set_from_handle(handle);
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
pub unsafe extern "C" fn mimi_set_to_json_f64(handle: SetHandle) -> *mut std::ffi::c_char {
    if handle == 0 {
        return alloc_c_string("[]");
    }
    // SAFETY: non-zero SetHandle.
    let set = set_from_handle(handle);
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
pub unsafe extern "C" fn mimi_set_to_json_string(handle: SetHandle) -> *mut std::ffi::c_char {
    if handle == 0 {
        return alloc_c_string("[]");
    }
    // SAFETY: non-zero SetHandle.
    let set = set_from_handle(handle);
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
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_set_from_json_f64(json: *const std::ffi::c_char) -> SetHandle {
    let handle = mimi_set_new();
    if handle == 0 || json.is_null() {
        return handle;
    }
    // SAFETY: non-null JSON C string from codegen.
    let s = unsafe { cstr_to_string(json) };
    // audit-wave1: internal consumer keeps sentinel behavior via _try variant.
    let len = json_array_length_try(json);
    if len <= 0 {
        return handle;
    }
    const MAX: i64 = 1_000_000;
    let n = len.min(MAX);
    for i in 0..n {
        let elem = json_get_element_try(json, i).unwrap_or(std::ptr::null_mut());
        if elem.is_null() {
            continue;
        }
        // SAFETY: elem is a heap C string from json_get_element_try.
        let es = unsafe { cstr_to_string(elem) };
        let bits = es.trim().parse::<f64>().unwrap_or(0.0).to_bits() as i64;
        mimi_free(elem as *mut std::ffi::c_void);
        mimi_set_insert(handle, bits as SetValueHandle);
    }
    let _ = s;
    handle
}

/// Display form `Set{1.5, 2}` for f64-bit sets (sorted by bit pattern / float value).
#[no_mangle]
pub unsafe extern "C" fn mimi_set_to_display_f64(handle: SetHandle) -> *mut std::ffi::c_char {
    if handle == 0 {
        return alloc_c_string("Set{}");
    }
    // SAFETY: non-zero SetHandle.
    let set = set_from_handle(handle);
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
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_set_from_json_string(json: *const std::ffi::c_char) -> SetHandle {
    let handle = mimi_set_new();
    if handle == 0 || json.is_null() {
        return handle;
    }
    // SAFETY: non-null JSON C string from codegen.
    let s = unsafe { cstr_to_string(json) };
    // audit-wave1: internal consumer keeps sentinel behavior via _try variant.
    let len = json_array_length_try(json);
    if len <= 0 {
        return handle;
    }
    const MAX: i64 = 1_000_000;
    let n = len.min(MAX);
    for i in 0..n {
        let elem = json_get_element_try(json, i).unwrap_or(std::ptr::null_mut());
        if elem.is_null() {
            continue;
        }
        // SAFETY: elem is a heap C string from json_get_element_try.
        let es = unsafe { cstr_to_string(elem) };
        // Strip surrounding quotes if present (json_get_element may return quoted).
        let body = es.trim().trim_matches('"');
        let v =
            unsafe { mimi_str_clone(body.as_ptr() as *const std::ffi::c_char, body.len() as i64) };
        mimi_free(elem as *mut std::ffi::c_void);
        mimi_set_insert(handle, v as SetValueHandle);
    }
    let _ = s;
    handle
}

/// Display form `Set{a, b}` for string-valued sets (sorted by string content).
#[no_mangle]
pub unsafe extern "C" fn mimi_set_to_display_string(handle: SetHandle) -> *mut std::ffi::c_char {
    if handle == 0 {
        return alloc_c_string("Set{}");
    }
    // SAFETY: non-zero SetHandle.
    let set = set_from_handle(handle);
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
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_set_from_json_i64(json: *const std::ffi::c_char) -> SetHandle {
    let handle = mimi_set_new();
    if handle == 0 || json.is_null() {
        return handle;
    }
    // SAFETY: non-null JSON C string from codegen.
    let s = unsafe { cstr_to_string(json) };
    // audit-wave1: internal consumer keeps sentinel behavior via _try variant.
    let len = json_array_length_try(json);
    if len <= 0 {
        return handle;
    }
    const MAX: i64 = 1_000_000;
    let n = len.min(MAX);
    for i in 0..n {
        let elem = json_get_element_try(json, i).unwrap_or(std::ptr::null_mut());
        if elem.is_null() {
            continue;
        }
        let v = unsafe { mimi_json_as_i64(elem) };
        // SAFETY: `elem` is non-null (null-checked above) and was allocated by `json_get_element_try` which uses `libc::malloc`
        // Free the element string allocated by json_get_element_try.
        mimi_free(elem as *mut std::ffi::c_void);
        mimi_set_insert(handle, v as SetValueHandle);
    }
    let _ = s;
    handle
}

#[no_mangle]
pub unsafe extern "C" fn mimi_set_destroy(handle: SetHandle) {
    let _ = handle::set_destroy(handle);
}

#[no_mangle]
pub unsafe extern "C" fn mimi_set_insert(handle: SetHandle, value: SetValueHandle) -> SetHandle {
    if handle == 0 {
        return handle;
    }
    // SAFETY: handle validated by `set_from_handle`; deref is in a single scope.
    unsafe {
        set_from_handle(handle).inner.insert(value);
    }
    handle
}

#[no_mangle]
pub unsafe extern "C" fn mimi_set_contains(handle: SetHandle, value: SetValueHandle) -> i64 {
    if handle == 0 {
        return 0;
    }
    // SAFETY: handle validated by `set_from_handle`; deref is in a single scope.
    unsafe { set_from_handle(handle).inner.contains(&value) as i64 }
}

#[no_mangle]
pub unsafe extern "C" fn mimi_set_remove(handle: SetHandle, value: SetValueHandle) -> SetHandle {
    if handle == 0 {
        return handle;
    }
    // SAFETY: handle validated by `set_from_handle`; deref is in a single scope.
    unsafe {
        set_from_handle(handle).inner.remove(&value);
    }
    handle
}

#[no_mangle]
pub unsafe extern "C" fn mimi_set_size(handle: SetHandle) -> i64 {
    if handle == 0 {
        return 0;
    }
    // SAFETY: handle validated by `set_from_handle`; deref is in a single scope.
    unsafe { set_from_handle(handle).inner.len() as i64 }
}

///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_set_to_list(
    handle: SetHandle,
    out_len: *mut i64,
) -> *mut SetValueHandle {
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
    let set = set_from_handle(handle);
    let len = set.inner.len() as i64;
    // SAFETY: `out_len` was checked non-null above.
    unsafe {
        *out_len = len;
    }
    if len == 0 {
        return std::ptr::null_mut();
    }
    let vec: Vec<SetValueHandle> = set.inner.iter().copied().collect();
    // audit-wave1 (RT-C5): shrink_to_fit is only a HINT — relying on it to make
    // len == capacity before a from_raw_parts reconstruction is UB. Convert to a
    // boxed slice, which is guaranteed to be an exact-size allocation.
    let boxed: Box<[SetValueHandle]> = vec.into_boxed_slice();
    let ptr = boxed.as_ptr() as *mut SetValueHandle;
    std::mem::forget(boxed); // ownership transferred to caller
    ptr
}

/// C9 fix: Free a SetValueHandle array returned by `mimi_set_to_list`.
/// Must be called with the pointer and length as returned by `mimi_set_to_list`.
/// Null pointer is a no-op.
///
/// # Safety
/// `ptr` must be null or the exact pointer/length pair returned by
/// `mimi_set_to_list`, and must not be freed more than once.
#[no_mangle]
pub unsafe extern "C" fn mimi_set_list_free(ptr: *mut SetValueHandle, len: i64) {
    if ptr.is_null() || len <= 0 {
        return;
    }
    // audit-wave1 (RT-C5): mimi_set_to_list hands out a boxed-slice
    // allocation (exact size, guaranteed). Reconstruct the `Box<[T]>` from the
    // thin pointer + length and drop it.
    // SAFETY: `ptr` came from `mimi_set_to_list`, which forgets a
    // `Box<[SetValueHandle]>` of exactly `len` elements (the precondition for
    // handing the pointer to the caller). The caller guarantees it passes back
    // the same pointer/length pair. `SetValueHandle` has no custom Drop.
    // `ptr` is non-null and `len > 0` (checked above).
    unsafe {
        let raw_slice: *mut [SetValueHandle] =
            std::ptr::slice_from_raw_parts_mut(ptr, len as usize);
        drop(Box::from_raw(raw_slice));
    }
}

mod regex;

// ─── Sort helpers ────────────────────────────────────────────────

/// Sorts an f64 list in place (ascending). Uses Rust's `sort_unstable_by`
/// for O(n log n) performance instead of the original O(n²) bubble sort.
///
/// # Safety
/// When `count > 1`, `data` must point to at least `count * 8` writable,
/// well-aligned bytes.
#[no_mangle]
pub unsafe extern "C" fn mimi_sort_f64_inplace(data: *mut u8, count: i64) {
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
///
/// # Safety
/// When `count > 1`, `data` must point to at least `count` valid `*mut c_char`
/// slots, and every non-null slot must point to a readable NUL-terminated string.
#[no_mangle]
pub unsafe extern "C" fn mimi_sort_str_inplace(data: *mut *mut std::ffi::c_char, count: i64) {
    if data.is_null() || count <= 1 {
        return;
    }
    let n = count as usize;
    // SAFETY: the pointer is non-null and points to at least `n` initialized elements
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
            // SAFETY: both slots are current fat string boxes (or rejected
            // as non-fat by cmp_fat_slots).
            unsafe { list_string::cmp_fat_slots(*a, *b) }
        }
    });
}

mod net;

// ---------------------------------------------------------------------------
// JSON FFI serialization
// ---------------------------------------------------------------------------

/// Serialize an array of i64/f64/string handles to JSON.
///
/// # Safety
/// `data` must be null or a readable, `align_of::<i64>()`-aligned pointer to
/// at least `len` `ValueHandle`-sized elements. `elem_type` selects how each
/// element is decoded and must match the actual element kind.
#[no_mangle]
pub unsafe extern "C" fn mimi_json_serialize(
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

///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_serialize(
    data: *mut std::ffi::c_void,
    len: i64,
) -> *mut std::ffi::c_char {
    // SAFETY: `data`/`len` are forwarded as-is from the caller; this ABI
    // entry point itself is `unsafe` because it dereferences raw slice data.
    unsafe { mimi_json_serialize(data, len, 0) }
}

///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_json_deserialize(
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
                let mut oversized = false;
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
                        // Oversized string: skip past closing quote and treat
                        // the whole parse as failed instead of silently storing
                        // a truncated value (batch4-05 P2-1).
                        oversized = true;
                        while pos < bytes.len() && bytes[pos] != b'"' {
                            pos += 1;
                        }
                        break;
                    }
                }
                if oversized {
                    for j in 0..idx {
                        let p = data[j as usize] as *mut std::ffi::c_char;
                        if !p.is_null() {
                            // SAFETY: slot holds a C string allocated by
                            // alloc_c_string_from_bytes earlier in this loop.
                            mimi_free(p as *mut std::ffi::c_void);
                        }
                    }
                    if !out_len.is_null() {
                        // SAFETY: `out_len` was checked non-null above.
                        unsafe { *out_len = 0 };
                    }
                    return std::ptr::null_mut();
                }
                // M19 fix: unescape JSON escape sequences (\n, \", \\, \uXXXX, etc.)
                let end = usize::min(pos, bytes.len());
                let raw_bytes = bytes[start..end].to_vec();
                // audit-wave1: json_unescape now fails (serde parity) on bad
                // \uXXXX / lone surrogates. Treat as a JSON parse failure:
                // free the already-allocated element strings, out_len=0, null.
                let unescaped = match json_unescape(&raw_bytes) {
                    Some(u) => u,
                    None => {
                        for j in 0..idx {
                            let p = data[j as usize] as *mut std::ffi::c_char;
                            if !p.is_null() {
                                // SAFETY: slot holds a C string allocated by
                                // alloc_c_string_from_bytes earlier in this loop.
                                mimi_free(p as *mut std::ffi::c_void);
                            }
                        }
                        if !out_len.is_null() {
                            // SAFETY: `out_len` was checked non-null above.
                            unsafe { *out_len = 0 };
                        }
                        return std::ptr::null_mut();
                    }
                };
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

    // RT-H3 (audit-wave1): truncate to the filled prefix and hand out a boxed
    // slice — an exact-size allocation by construction. The old code relied on
    // shrink_to_fit (a HINT, not a guarantee) before a capacity-sensitive
    // reconstruction; reconstructing now needs no capacity assumption.
    data.truncate(idx as usize);
    let out = idx;
    if !out_len.is_null() {
        // SAFETY: `out_len` was checked non-null above.
        unsafe {
            *out_len = out;
        }
    }
    // Empty result: no heap buffer (an empty boxed slice has a dangling
    // non-null pointer that must never cross the FFI boundary).
    if out == 0 {
        return std::ptr::null_mut();
    }
    let boxed: Box<[i64]> = data.into_boxed_slice();
    let result = boxed.as_ptr() as *mut i64;
    std::mem::forget(boxed); // ownership transferred to caller
    result as *mut std::ffi::c_void
}

/// C11: Free a buffer returned by mimi_json_deserialize / mimi_list_deserialize.
/// Reconstructs the boxed slice and drops it, freeing both the data buffer and
/// any heap-allocated string pointers (elem_type==2).
///
/// RT-H3 (audit-wave1): the producer hands out a boxed slice (exact-size
/// allocation), so reconstruction reads `len` elements and makes no capacity
/// assumption.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_json_deserialize_free(
    buf: *mut std::ffi::c_void,
    len: i64,
    elem_type: i64,
) {
    if buf.is_null() || len <= 0 {
        return;
    }
    let count = len as usize;
    // SAFETY: `buf` was created by a prior mimi_json_deserialize call with
    // matching `len` and `elem_type`; the caller guarantees it passes back the
    // same pointer/length pair. The producer allocated an exact-size boxed
    // slice of `count` i64 (or bit-cast f64 / C-string pointers).
    unsafe {
        let ptr = buf as *mut i64;
        // If this was a string-typed deserialization, free each C string first.
        if elem_type == 2 {
            for i in 0..count {
                let p = *ptr.add(i) as *mut std::ffi::c_char;
                if !p.is_null() {
                    mimi_free(p as *mut std::ffi::c_void);
                }
            }
        }
        // Drop the boxed slice (i64 is trivially copy; strings were already
        // freed above, so no element destructor needs to run).
        let raw_slice: *mut [i64] = std::ptr::slice_from_raw_parts_mut(ptr, count);
        drop(Box::from_raw(raw_slice));
    }
}

///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_list_deserialize(
    json: *const std::ffi::c_char,
    out_len: *mut i64,
) -> *mut std::ffi::c_void {
    mimi_json_deserialize(json, out_len, 0)
}

/// Serialize a tuple-like flat array to JSON.
///
/// # Safety
/// `values` must be null or a valid pointer to at least `count` i64 values.
/// If `elem_types` is non-null it must point to at least `count` i64 tags.
#[no_mangle]
pub unsafe extern "C" fn mimi_tuple_serialize(
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

///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_tuple_deserialize(
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
                // audit-wave1: json_unescape fails (serde parity) on bad
                // \uXXXX / lone surrogates. Treat as a parse failure: free the
                // string elements already written into out_values, return -1.
                let unescaped = match json_unescape(&raw_bytes) {
                    Some(u) => u,
                    None => {
                        for j in 0..idx {
                            if (j as usize) < types.len() && types[j as usize] == 2 {
                                // SAFETY: out_values is the caller's array with
                                // `count` entries; slot j (< idx <= count) of a
                                // string-typed element holds a C string allocated
                                // by alloc_c_string_from_bytes earlier in this call.
                                let p = unsafe {
                                    *out_values.offset(j as isize) as *mut std::ffi::c_char
                                };
                                if !p.is_null() {
                                    // SAFETY: same slot contract as the load above.
                                    mimi_free(p as *mut std::ffi::c_void);
                                }
                            }
                        }
                        return -1;
                    }
                };
                // audit-wave1 (audit §10 MEDIUM): allocate a proper "" string
                // for empty values — the list deserializer allocates, so the
                // tuple path must too (uniform owning-pointer contract; writing
                // 0/NULL made codegen treat "" as "no string" → puts(NULL) UB).
                // SAFETY: out_values is the caller's array with `count`
                // entries; idx is bounds-checked above against count.
                // The store overwrites a previously-written slot in
                // the same array.
                unsafe {
                    *out_values.offset(idx as isize) = alloc_c_string_from_bytes(&unescaped) as i64;
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
                    // SAFETY: the offset is within bounds of `out_values`'s allocated buffer
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
// R-1 (0.31.52): store function pointer as opaque data pointer via usize round-trip.
// AtomicPtr<ErrorHandler> was UB — it stored a fn ptr as *mut ErrorHandler
// (pointer-to-fn-ptr), then dereferenced it as if it pointed to an ErrorHandler
// value. On architectures where fn ptrs ≠ data ptrs this is SIGSEGV.
// Fix: AtomicPtr<()> stores the fn ptr bits as a data pointer; we round-trip
// through usize to call it. Runtime is exempt from Flow transmute ban (§20.2).
static ERROR_HANDLER: AtomicPtr<()> = AtomicPtr::new(std::ptr::null_mut());

#[no_mangle]
pub extern "C" fn mimi_runtime_set_error_handler(handler: Option<ErrorHandler>) {
    let ptr: *mut () = match handler {
        Some(f) => f as usize as *mut (),
        None => std::ptr::null_mut(),
    };
    ERROR_HANDLER.store(ptr, Ordering::Release);
}

///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_runtime_abort(msg: *const std::ffi::c_char) -> ! {
    // P3-19 fix: write to stderr fd using raw syscall (async-signal-safe),
    // instead of eprintln!() which acquires locks that may deadlock in signal context.
    extern "C" {
        fn write(fd: i32, buf: *const std::ffi::c_void, count: usize) -> isize;
    }
    // Neutral prefix: this abort path serves contract violations, list
    // out-of-bounds, OOM, and string range checks alike. The category lives in
    // the caller-baked message (contract messages carry their own `[E0808]`
    // code, span, source line and disable-hint since 0.34.34). The old
    // "[FFI contract violation]" label was wrong for every non-FFI caller, and
    // the fixed `--skip-verify-ffi` hint was wrong for `--verify-contracts`
    // binaries — both removed.
    const PREFIX: &[u8] = b"[mimi] ";
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
    }

    let handler_ptr = ERROR_HANDLER.load(Ordering::Acquire);
    if !handler_ptr.is_null() {
        ERROR_HANDLER.store(std::ptr::null_mut(), Ordering::Release);
        // R-1 (0.31.52): round-trip through usize to recover the fn pointer.
        // SAFETY: handler_ptr was stored from a valid ErrorHandler fn pointer
        // via `f as usize as *mut ()`. The usize→fn transmute is valid because
        // we only store values that originated from `Some(f)`.
        let handler: ErrorHandler = unsafe { std::mem::transmute(handler_ptr as usize) };
        // SAFETY: calling the registered error handler with the validated message pointer.
        unsafe { handler(msg) };
        std::process::abort();
    }

    std::process::abort();
}

// ── SD-7/SD-8 (0.31.51a): Arithmetic trap functions ──────────────────
// Trap = synchronous arithmetic failure (E08xx). These are NOT Faults
// (Flow state machine invariant violations). Different channels.

// U2 (0.35.44): shared trap wording, `include!`d so the standalone runtime
// (compiled with rustc, no `crate::diagnostic`) and the VM share one source.
mod trap {
    include!("../diagnostic/trap_msgs.rs");
}
// E-code strings matching `diagnostic::codes::E08xx` (standalone runtime can't
// reach the crate module, so they are mirrored here; the VM side reads the
// codes.rs constants directly). Keep in sync — see docs/error-codes.md.
const E0800: &str = "E0800";
const E0801: &str = "E0801";
const E0802: &str = "E0802";
const E0813: &str = "E0813";

/// Write a static byte slice to stderr (async-signal-safe; no allocation).
#[inline]
fn trap_write_static(bytes: &'static [u8]) {
    extern "C" {
        fn write(fd: i32, buf: *const std::ffi::c_void, count: usize) -> isize;
    }
    // SAFETY: writing a static byte buffer to stderr (fd 2) is async-signal-safe.
    unsafe {
        let _ = write(2, bytes.as_ptr() as *const std::ffi::c_void, bytes.len());
    }
}

/// Write `len` bytes from a raw pointer to stderr (async-signal-safe).
#[inline]
fn trap_write_raw(bytes: *const u8, len: usize) {
    extern "C" {
        fn write(fd: i32, buf: *const std::ffi::c_void, count: usize) -> isize;
    }
    // SAFETY: caller guarantees `bytes` points to `len` readable bytes; write
    // to stderr (fd 2) is async-signal-safe.
    unsafe {
        let _ = write(2, bytes as *const std::ffi::c_void, len);
    }
}

/// Write `[<code>] ` (bracketed E-code prefix).
#[inline]
fn trap_write_code(code: &'static str) {
    trap_write_static(b"[");
    trap_write_static(code.as_bytes());
    trap_write_static(b"] ");
}

/// SD-7: Integer overflow trap. Called when checked arithmetic detects
/// overflow in add/sub/mul. Prints diagnostic and aborts.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_trap_overflow(op: *const std::ffi::c_char) -> ! {
    // M1 (audit-codegen 2026-08-03): integer overflow is E0802 per
    // docs/error-codes.md (E0801 is reserved for division by zero); the
    // bytecode VM's IntegerOverflow also maps to E0802.
    const PREFIX: &[u8] = trap::INT_OVERFLOW_PREFIX.as_bytes();
    // 0.34.34 (docs/diagnostics.md §2): hints ride the single dense line as
    // the `| hint:` field — no separate "Hint:" line.
    const SUFFIX: &[u8] =
        b" | hint: use wrapping_add/wrapping_sub/wrapping_mul for wrap-around semantics\n";
    trap_write_code(E0802);
    trap_write_static(PREFIX);
    if !op.is_null() {
        let mut len = 0usize;
        let base = op as *const u8;
        const MAX_MSG: usize = 64;
        // SAFETY: op points to a NUL-terminated static C string in the program
        // image; bounded read up to MAX_MSG before the NUL terminator.
        unsafe {
            while len < MAX_MSG && *base.add(len) != 0 {
                len += 1;
            }
        }
        trap_write_raw(op as *const u8, len);
    }
    trap_write_static(SUFFIX);
    std::process::abort();
}

/// SD-8: Division by zero trap. Called when integer division or modulo
/// has a zero divisor. Prints diagnostic and aborts.
#[no_mangle]
pub extern "C" fn mimi_trap_div_by_zero() -> ! {
    const MSG: &[u8] = trap::INT_DIV_BY_ZERO.as_bytes();
    trap_write_code(E0801);
    trap_write_static(MSG);
    trap_write_static(b"\n");
    std::process::abort();
}

/// SD-8: MIN/-1 division trap. Called when i32::MIN / -1 or i64::MIN / -1
/// is attempted (result overflows the signed range).
#[no_mangle]
pub extern "C" fn mimi_trap_div_overflow() -> ! {
    // M1: MIN/-1 division overflow is E0802 (integer overflow), not E0801.
    const MSG: &[u8] = trap::INT_DIV_OVERFLOW.as_bytes();
    trap_write_code(E0802);
    trap_write_static(MSG);
    trap_write_static(b"\n");
    std::process::abort();
}

/// 0.36.10 (裁决 6 follow-up): `recover`/`reset` called on a transition result
/// that DECLARED faultability (`-> S | Fault`) but whose runtime tag is NOT
/// Fault — the value is a live state, and neither backend can recover/reset
/// it. Mirrors the bytecode VM's flow-transition miss text exactly
/// ("no transition {flow}::{verb} from state {state}", generic E0800).
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_trap_no_flow_transition(
    flow: *const std::ffi::c_char,
    verb: *const std::ffi::c_char,
    from_state: *const std::ffi::c_char,
) -> ! {
    trap_write_code(E0800);
    trap_write_static(b"no transition ");
    trap_write_cstr_bounded(flow);
    trap_write_static(b"::");
    trap_write_cstr_bounded(verb);
    trap_write_static(b" from state ");
    trap_write_cstr_bounded(from_state);
    trap_write_static(b"\n");
    std::process::abort();
}

/// Bounded C-string writer for the trap helpers (same MAX_MSG discipline as
/// `mimi_trap_overflow`): the string is a NUL-terminated static in the
/// program image, so a bounded scan before the terminator is safe.
fn trap_write_cstr_bounded(ptr: *const std::ffi::c_char) {
    if ptr.is_null() {
        return;
    }
    let mut len = 0usize;
    let base = ptr as *const u8;
    const MAX_MSG: usize = 64;
    // SAFETY: ptr points to a NUL-terminated static C string in the program
    // image; bounded read up to MAX_MSG before the NUL terminator.
    unsafe {
        while len < MAX_MSG && *base.add(len) != 0 {
            len += 1;
        }
    }
    trap_write_raw(ptr as *const u8, len);
}

/// SD-9 (0.31.51b): Float finiteness trap. Called when a float operation
/// produces NaN or Infinity. Prints diagnostic and aborts.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_trap_float_not_finite(op: *const std::ffi::c_char) -> ! {
    const PREFIX: &[u8] = trap::FLOAT_NOT_FINITE_PREFIX.as_bytes();
    // 0.34.34 (docs/diagnostics.md §2): hints ride the single dense line as
    // the `| hint:` field — no separate "Hint:" line.
    const SUFFIX: &[u8] =
        b" | hint: use ieee_float { } block for IEEE 754 semantics (post-0.31.51b)\n";
    trap_write_code(E0813);
    trap_write_static(PREFIX);
    if !op.is_null() {
        let mut len = 0usize;
        let base = op as *const u8;
        const MAX_MSG: usize = 64;
        // SAFETY: op points to a NUL-terminated static C string in the program
        // image; bounded read up to MAX_MSG before the NUL terminator.
        unsafe {
            while len < MAX_MSG && *base.add(len) != 0 {
                len += 1;
            }
        }
        trap_write_raw(op as *const u8, len);
    }
    trap_write_static(SUFFIX);
    std::process::abort();
}

/// 0.35.7-fix: literal pattern match assertion. The legacy pattern binder
/// (codegen/func/pattern.rs, `PatternKind::Literal`) emits a call to
/// `mimi_runtime_assert(cond, msg)` for literal sub-patterns — e.g.
/// `Bool(true) => ...`, `Some(0) => ...` — but the symbol was declared with
/// no definition, so any program with such a pattern failed to link.
/// Pattern-match failures are a language-level trap (E0801 family), so on
/// failure we print the message and abort — never silently fall through.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_runtime_assert(cond: bool, msg: *const std::ffi::c_char) {
    if cond {
        return;
    }
    extern "C" {
        fn write(fd: i32, buf: *const std::ffi::c_void, count: usize) -> isize;
    }
    const PREFIX: &[u8] = b"[E0801] pattern match failed: ";
    // SAFETY: writing static byte buffers to stderr (fd 2) is async-signal-safe.
    unsafe {
        let _ = write(2, PREFIX.as_ptr() as *const std::ffi::c_void, PREFIX.len());
        if !msg.is_null() {
            let mut len = 0usize;
            let base = msg as *const u8;
            const MAX_MSG: usize = 128;
            while len < MAX_MSG && *base.add(len) != 0 {
                len += 1;
            }
            let _ = write(2, msg as *const std::ffi::c_void, len);
        }
        let _ = write(2, b"\n".as_ptr() as *const std::ffi::c_void, 1);
    }
    std::process::abort();
}

/// v0.29.38-fix: inject_fault(state_name) — prints a message and aborts.
/// In the interp path, inject_fault constructs a proper Fault record with
/// SystemTrace. In codegen, we cannot easily construct the record at runtime,
/// so we print a diagnostic and abort. This ensures test programs that rely
/// on inject_fault do not silently continue with a bogus value.
///
/// audit-wave1 (audit §10 LOW): the doc said "aborts" but the body returned a
/// -1 sentinel. Callers (and this doc) expect abort semantics, so make it
/// actually abort. Codegen rejects `inject_fault` at compile time
/// (`builtins/mod.rs`: "cannot construct a Fault/SystemTrace value"), so no
/// generated code depends on the old -1 return; the interp path constructs the
/// Fault record itself and never calls this symbol.
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_inject_fault(state_name: *const std::ffi::c_char) -> i64 {
    let state = if state_name.is_null() {
        "unknown".to_string()
    } else {
        // SAFETY: `state_name` points to a valid null-terminated C string
        unsafe { std::ffi::CStr::from_ptr(state_name) }
            .to_string_lossy()
            .into_owned()
    };
    eprintln!(
        "[mimi runtime] inject_fault: injecting Fault into state '{}' — aborting",
        state
    );
    // Abort as documented (fail loud; never hand back a bogus sentinel).
    std::process::abort();
}

/// v0.29.38-fix: assert_state(actual_state_cstr, expected_state_cstr)
/// Compares two C strings; if they differ, prints an error and aborts.
/// If `actual_state` is null, the check is skipped (codegen cannot extract
/// the state name at runtime — the interp path does the full check).
///
/// # Safety
/// Pointer arguments must be valid for the documented C ABI
/// (live runtime objects, NUL-terminated strings, or sized arrays
/// with matching length arguments).
#[no_mangle]
pub unsafe extern "C" fn mimi_assert_state(
    actual_state: *const std::ffi::c_char,
    expected_state: *const std::ffi::c_char,
) -> i64 {
    // Skip check if actual_state is null (codegen path limitation)
    if actual_state.is_null() {
        return 0;
    }
    // SAFETY: `actual_state` points to a valid null-terminated C string
    let actual = unsafe { std::ffi::CStr::from_ptr(actual_state) }
        .to_string_lossy()
        .into_owned();
    let expected = if expected_state.is_null() {
        "(null)".to_string()
    } else {
        // SAFETY: `expected_state` points to a valid null-terminated C string
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

mod capability;
#[cfg(not(standalone))]
pub use capability::*;

mod future;

// pub(crate): the bytecode VM's exec builtins call `run_exec_capped` through
// this module so both backends cap subprocess output identically (H12).
pub(crate) mod fs;

mod env;

mod crypto;
#[cfg(not(standalone))]
pub use crypto::mimi_runtime_buf_nul_terminate;
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
        unsafe {
            assert_eq!(mimi_quote_tag(ptr), -1);
            assert_eq!(mimi_quote_data0(ptr), 0);
            assert_eq!(mimi_quote_argc(ptr), 0);
            assert!(mimi_quote_list_child(ptr, 0).is_null());
        }
    }

    #[test]
    fn quote_snapshot_accessors_reject_dropped_handle() {
        let node = mimi_quote_new_leaf(QuotedAstTag::QastInt as i32, 42);
        unsafe {
            assert_eq!(mimi_quote_tag(node), QuotedAstTag::QastInt as i32);
            mimi_quote_drop(node);
            assert_eq!(mimi_quote_tag(node), -1);
            assert_eq!(mimi_quote_data0(node), 0);
            assert_eq!(mimi_quote_argc(node), 0);
            assert!(mimi_quote_list_child(node, 0).is_null());
        }
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
        assert_eq!(unsafe { mimi_map_size(h) }, 0);
        unsafe { mimi_map_destroy(h) };
        // Second destroy must not free again (would be double-free).
        unsafe { mimi_map_destroy(h) };
        unsafe { mimi_map_destroy(0) };
    }

    #[test]
    fn set_double_destroy_is_noop() {
        let h = mimi_set_new();
        assert_ne!(h, 0);
        unsafe { mimi_set_destroy(h) };
        unsafe { mimi_set_destroy(h) };
        unsafe { mimi_set_destroy(0) };
    }

    #[test]
    fn map_ops_on_live_handle_work() {
        let h = mimi_map_new();
        let key = b"k\0".as_ptr() as *const std::ffi::c_char;
        unsafe {
            mimi_map_set(h, key, 42);
        }
        unsafe {
            assert_eq!(mimi_map_has_key(h, key), 1);
            assert_eq!(mimi_map_get(h, key), 42);
        }
        assert_eq!(unsafe { mimi_map_size(h) }, 1);
        unsafe { mimi_map_destroy(h) };
    }

    #[test]
    fn set_insert_on_live_handle_works() {
        let h = mimi_set_new();
        let h2 = unsafe { mimi_set_insert(h, 7) };
        assert_eq!(h, h2);
        unsafe { mimi_set_destroy(h) };
    }
}

// ---------------------------------------------------------------------------
// audit-wave1 regression tests (devdocs/full-audit-2026-08-05.md §10)
// Abort-path checks (fail-loud json accessors, mimi_inject_fault,
// str_substring_clamp start>end) live in the central e2e abort harness;
// only non-aborting paths are exercised in-process here.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod audit_wave1_tests {
    use super::*;

    // ── Fix #7: JSON \uXXXX surrogate handling ────────────────────────

    #[test]
    fn json_unescape_combines_surrogate_pair() {
        // U+1F600 GRINNING FACE = 😀
        let out = json_unescape(br"\ud83d\ude00").expect("valid pair must decode");
        assert_eq!(out, "\u{1F600}".as_bytes());
    }

    #[test]
    fn json_unescape_decodes_bmp_escape() {
        let out = json_unescape(br"\u0041BC").expect("BMP escape must decode");
        assert_eq!(out, b"ABC");
    }

    #[test]
    fn json_unescape_rejects_lone_high_surrogate() {
        assert!(json_unescape(br"\ud83dabc").is_none());
        assert!(json_unescape(br"\ud83d").is_none());
        assert!(json_unescape(br"\ud800").is_none());
        assert!(json_unescape(br"\udbff").is_none());
    }

    #[test]
    fn json_unescape_rejects_lone_low_surrogate() {
        assert!(json_unescape(br"\udc00").is_none());
        assert!(json_unescape(br"\udfff").is_none());
    }

    #[test]
    fn json_unescape_rejects_high_surrogate_followed_by_high() {
        // Two high surrogates in a row: the second is not a valid low.
        assert!(json_unescape(br"\ud83d\ud83d").is_none());
    }

    #[test]
    fn json_unescape_rejects_malformed_hex() {
        // The old code fell back to "0000" → NUL; serde parity is failure.
        assert!(json_unescape(br"\uzzzz").is_none());
        assert!(json_unescape(br"\u12").is_none()); // too short
    }

    #[test]
    fn json_unescape_rejects_dangling_backslash() {
        assert!(json_unescape(b"abc\\").is_none());
    }

    #[test]
    fn safe_c_string_from_handle_reads_long_strings_beyond_256() {
        let text = "x".repeat(600);
        let c = alloc_c_string(&text);
        let decoded =
            safe_c_string_from_handle(c as i64).expect("long C string should be readable");
        assert_eq!(decoded, text);
        // SAFETY: c was returned by alloc_c_string (mimi_alloc) and is freed once.
        mimi_free(c as *mut _);
    }

    #[test]
    fn safe_c_string_from_handle_reads_multipage_strings() {
        // Regression for batch4-04 P1-1: strings spanning several pages were
        // previously truncated by a 4 KiB bounded scan.
        let text = "z".repeat(5000);
        let c = alloc_c_string(&text);
        let decoded =
            safe_c_string_from_handle(c as i64).expect("multipage C string should be readable");
        assert_eq!(decoded, text);
        // SAFETY: c was returned by alloc_c_string (mimi_alloc) and is freed once.
        mimi_free(c as *mut _);
    }

    #[test]
    fn safe_c_string_from_ptr_accepts_byte_aligned_storage() {
        // Native string literals are valid C strings but need not be 8-byte
        // aligned. Map keys use this ABI-declared pointer path rather than the
        // aligned Any pointer-vs-integer heuristic.
        let mut bytes = Vec::from([0u8]);
        bytes.extend_from_slice(b"key\0");
        let ptr = unsafe { bytes.as_ptr().add(1) } as *const std::ffi::c_char;
        assert_eq!(safe_c_string_from_ptr(ptr).as_deref(), Some("key"));
    }

    #[test]
    fn mimi_any_to_string_reads_multipage_strings() {
        // batch4-04 P1-1: the untyped Any renderer must also scan across pages.
        let text = "y".repeat(5000);
        let c = alloc_c_string(&text);
        // SAFETY: c is a valid runtime-allocated C string.
        let raw = unsafe { mimi_any_to_string(c as i64) };
        assert!(!raw.is_null());
        // SAFETY: raw is a valid runtime-allocated C string.
        let decoded = unsafe { cstr_to_string(raw) };
        assert_eq!(decoded, text);
        // SAFETY: both pointers were returned by alloc_c_string/mimi_any_to_string.
        mimi_free(c as *mut _);
        mimi_free(raw as *mut _);
    }

    #[test]
    fn json_decode_unicode_escape_reports_consumption() {
        let (_, consumed_pair) = json_decode_unicode_escape(b"d83d\\ude00", 0).unwrap();
        assert_eq!(consumed_pair, 10);
        let (_, consumed_bmp) = json_decode_unicode_escape(b"0041rest", 0).unwrap();
        assert_eq!(consumed_bmp, 4);
    }

    // ── Fix #3: decode_result_err_string validates inner pointer ──────

    #[test]
    fn decode_result_err_string_scalar_falls_back_to_decimal() {
        assert_eq!(decode_result_err_string(42), "42");
        assert_eq!(decode_result_err_string(-7), "-7");
    }

    #[test]
    fn decode_result_err_string_reads_valid_string_struct() {
        // Heap Mimi string struct {ptr, i64 len}.
        let payload: &[u8] = b"boom";
        let s = Box::new([payload.as_ptr() as i64, payload.len() as i64]);
        let addr = Box::into_raw(s) as *mut [i64; 2];
        let got = decode_result_err_string(addr as usize as i64);
        assert_eq!(got, "\"boom\"");
        // SAFETY: addr came from Box::into_raw just above.
        unsafe { drop(Box::from_raw(addr)) };
    }

    #[test]
    fn decode_result_err_string_unmapped_inner_is_sentinel_empty() {
        // Outer struct IS mapped (this box), but the inner ptr points at an
        // unmapped userspace hole. Old code dereferenced it blind (SIGSEGV);
        // new code must mincore-probe and return the sentinel "".
        let unmapped: usize = 0x0000_7000_0000_0000; // canonical, unmapped hole
        let s = Box::new([unmapped as i64, 16i64]);
        let addr = Box::into_raw(s) as *mut [i64; 2];
        let got = decode_result_err_string(addr as usize as i64);
        assert_eq!(got, "\"\"");
        // SAFETY: addr came from Box::into_raw just above.
        unsafe { drop(Box::from_raw(addr)) };
    }

    #[test]
    fn decode_result_err_string_absurd_len_never_reads_target() {
        // len outside the plausibility bound → C-string fallback / scalar,
        // never a 900k-byte probe-read of arbitrary memory.
        let s = Box::new([0x1000i64, 999_999_999i64]);
        let addr = Box::into_raw(s) as *mut [i64; 2];
        let got = decode_result_err_string(addr as usize as i64);
        // Whatever the fallback resolves to, it must not be the struct read.
        assert!(!got.contains('\u{1000}'));
        // SAFETY: addr came from Box::into_raw just above.
        unsafe { drop(Box::from_raw(addr)) };
    }

    // ── Fix #1/#2: from_json list builders (Box allocator + kind) ─────

    #[test]
    fn list_from_json_builders_set_element_kind_and_free_cleanly() {
        // Option of product: elements are malloc'd packs → Record.
        let l1 =
            unsafe { mimi_list_from_json_option_product_i64(b"[null,[7,8]]\0".as_ptr() as _, 2) };
        assert!(!l1.is_null());
        unsafe {
            assert_eq!(mimi_list_element_kind(l1), ListElementKind::Record as i8);
            mimi_list_free(l1, true);
        }

        // Set of product: elements are SetHandles → Set.
        let l2 =
            unsafe { mimi_list_from_json_set_product_i64(b"[[1,2],[3,4]]\0".as_ptr() as _, 2) };
        assert!(!l2.is_null());
        unsafe {
            assert_eq!(mimi_list_element_kind(l2), ListElementKind::Set as i8);
        }
        // Destroy the set handles stored in the data array before freeing.
        // SAFETY: l2 is non-null with a valid {len, data} layout just built.
        unsafe {
            let lst = &*l2;
            for i in 0..lst.len {
                let h = *(lst.data as *const i64).offset(i as isize) as SetHandle;
                if h != 0 {
                    mimi_set_destroy(h);
                }
            }
        }
        unsafe {
            mimi_list_free(l2, false);
        }

        // Map of product: elements are MapHandles → Map.
        let l3 = unsafe {
            mimi_list_from_json_map_product_i64(b"[{\"a\":1},{\"b\":2}]\0".as_ptr() as _, 1)
        };
        assert!(!l3.is_null());
        unsafe {
            assert_eq!(mimi_list_element_kind(l3), ListElementKind::Map as i8);
        }
        // SAFETY: same layout contract as above.
        unsafe {
            let lst = &*l3;
            for i in 0..lst.len {
                let h = *(lst.data as *const i64).offset(i as isize) as MapHandle;
                if h != 0 {
                    mimi_map_destroy(h);
                }
            }
        }
        unsafe {
            mimi_list_free(l3, false);
        }

        // Result of product: elements are malloc'd packs → Record.
        let l4 = unsafe {
            mimi_list_from_json_result_product_i64(
                b"[{\"Ok\":[1,2]},{\"Err\":\"e\"}]\0".as_ptr() as _,
                2,
            )
        };
        if !l4.is_null() {
            unsafe {
                assert_eq!(mimi_list_element_kind(l4), ListElementKind::Record as i8);
                mimi_list_free(l4, true);
            }
        }
    }

    #[test]
    fn list_from_json_builders_invalid_input_empty_with_kind() {
        // Malformed JSON must still yield a Box-allocated list with a valid
        // element_kind (the empty() path used to leave it uninitialized).
        let l = unsafe { mimi_list_from_json_option_product_i64(b"not json\0".as_ptr() as _, 2) };
        assert!(!l.is_null());
        unsafe {
            assert_eq!(mimi_list_element_kind(l), ListElementKind::Record as i8);
            mimi_list_free(l, true);
        }
    }

    // ── Fix #4: ptr+len string externs ────────────────────────────────

    fn owned_str(ptr: *mut std::ffi::c_char) -> String {
        assert!(!ptr.is_null());
        // SAFETY: alloc_c_string results are NUL-terminated heap strings.
        let s = unsafe { std::ffi::CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned();
        unsafe {
            mimi_string_free(ptr);
        }
        s
    }

    #[test]
    fn str_substring_clamp_matches_vm_function_form() {
        let s = b"hello";
        let p = s.as_ptr() as *const std::ffi::c_char;
        // In-range: identical to VM function form.
        assert_eq!(
            owned_str(unsafe { mimi_str_substring_clamp(p, 5, 1, 4) }),
            "ell"
        );
        // End beyond char count clamps to len (VM parity; method form aborts).
        assert_eq!(
            owned_str(unsafe { mimi_str_substring_clamp(p, 5, 2, 99) }),
            "llo"
        );
        // Start beyond char count clamps to len → empty slice.
        assert_eq!(
            owned_str(unsafe { mimi_str_substring_clamp(p, 5, 99, 100) }),
            ""
        );
        // Empty range.
        assert_eq!(
            owned_str(unsafe { mimi_str_substring_clamp(p, 5, 0, 0) }),
            ""
        );
        // Unicode chars (byte len 6, char count 5): indices are char-based.
        let u = "héllo"; // é is 2 bytes
        let up = u.as_ptr() as *const std::ffi::c_char;
        assert_eq!(
            owned_str(unsafe { mimi_str_substring_clamp(up, u.len() as i64, 1, 3) }),
            "él"
        );
    }

    #[test]
    fn str_to_upper_lower_full_unicode() {
        let s = b"Hello";
        let p = s.as_ptr() as *const std::ffi::c_char;
        assert_eq!(owned_str(unsafe { mimi_str_to_upper(p, 5) }), "HELLO");
        assert_eq!(owned_str(unsafe { mimi_str_to_lower(p, 5) }), "hello");
        // ß uppercases to SS (multi-char mapping — impossible byte-wise).
        let sharp = "Straße";
        let sp = sharp.as_ptr() as *const std::ffi::c_char;
        assert_eq!(
            owned_str(unsafe { mimi_str_to_upper(sp, sharp.len() as i64) }),
            "STRASSE"
        );
    }

    #[test]
    fn str_trim_is_unicode_aware() {
        let t = "\u{00A0}\t hi \n\u{00A0}"; // NBSP + ASCII whitespace
        let p = t.as_ptr() as *const std::ffi::c_char;
        assert_eq!(owned_str(unsafe { mimi_str_trim(p, t.len() as i64) }), "hi");
    }

    // ── Fix #6/#7: tuple deserialize empty string + surrogate pair ────

    #[test]
    fn tuple_deserialize_empty_string_allocates() {
        // Fix #6: "" must come back as an owning "" string, not NULL/0.
        let json = b"[\"\",\"abc\"]\0";
        let mut types: [i64; 2] = [2, 2];
        let mut out: [i64; 2] = [-1, -1];
        let n = unsafe {
            mimi_tuple_deserialize(json.as_ptr() as _, 2, types.as_mut_ptr(), out.as_mut_ptr())
        };
        assert_eq!(n, 2);
        for slot in out.iter() {
            assert_ne!(*slot, 0, "empty string must allocate, not write NULL");
            // SAFETY: each slot holds a C string allocated by the runtime.
            let c = unsafe { std::ffi::CStr::from_ptr(*slot as *const std::ffi::c_char) };
            let _ = c.to_string_lossy();
            // SAFETY: same allocation; matches mimi_json_deserialize_free's free.
            mimi_free(*slot as *mut std::ffi::c_void);
        }
        // First slot is exactly "".
        // (Re-run to inspect content without use-after-free.)
        let mut out2: [i64; 2] = [-1, -1];
        let _ = unsafe {
            mimi_tuple_deserialize(json.as_ptr() as _, 2, types.as_mut_ptr(), out2.as_mut_ptr())
        };
        // SAFETY: out2[0] is a fresh "" allocation from the runtime.
        let first = unsafe { std::ffi::CStr::from_ptr(out2[0] as *const std::ffi::c_char) };
        assert_eq!(first.to_string_lossy(), "");
        for slot in out2.iter() {
            // SAFETY: see above.
            mimi_free(*slot as *mut std::ffi::c_void);
        }
    }

    #[test]
    fn tuple_deserialize_surrogate_pair_and_lone_surrogate_fails() {
        let mut types: [i64; 1] = [2];
        let mut out: [i64; 1] = [-1];
        // Valid pair →😀
        let ok = b"[\"\\ud83d\\ude00\"]\0";
        let n = unsafe {
            mimi_tuple_deserialize(ok.as_ptr() as _, 1, types.as_mut_ptr(), out.as_mut_ptr())
        };
        assert_eq!(n, 1);
        // SAFETY: out[0] holds the runtime-allocated unescaped string.
        let s = unsafe { std::ffi::CStr::from_ptr(out[0] as *const std::ffi::c_char) }
            .to_string_lossy()
            .into_owned();
        assert_eq!(s, "\u{1F600}");
        // SAFETY: see above.
        mimi_free(out[0] as *mut std::ffi::c_void);

        // Lone surrogate → parse failure (-1).
        let bad = b"[\"\\ud800\"]\0";
        let mut out2: [i64; 1] = [-1];
        let n2 = unsafe {
            mimi_tuple_deserialize(bad.as_ptr() as _, 1, types.as_mut_ptr(), out2.as_mut_ptr())
        };
        assert_eq!(n2, -1);
    }

    // ── Fix #7+#8: list deserialize surrogates + boxed-slice free ─────

    #[test]
    fn json_deserialize_strings_and_free_via_boxed_slice() {
        let json = b"[\"a\",\"\\ud83d\\ude00\",\"c\"]\0";
        let mut len: i64 = -1;
        let buf = unsafe { mimi_json_deserialize(json.as_ptr() as _, &mut len as *mut i64, 2) };
        assert!(!buf.is_null());
        assert_eq!(len, 3);
        // Element 1 must be the combined surrogate pair.
        // SAFETY: buf is a boxed slice of `len` string-pointer i64 slots.
        unsafe {
            let ptr = buf as *mut i64;
            let mid = *ptr.add(1) as *const std::ffi::c_char;
            let s = std::ffi::CStr::from_ptr(mid).to_string_lossy().into_owned();
            assert_eq!(s, "\u{1F600}");
        }
        unsafe { mimi_json_deserialize_free(buf, len, 2) };
    }

    #[test]
    fn json_deserialize_lone_surrogate_fails_parse() {
        let json = b"[\"\\udc00\"]\0";
        let mut len: i64 = -1;
        let buf = unsafe { mimi_json_deserialize(json.as_ptr() as _, &mut len as *mut i64, 2) };
        assert!(buf.is_null(), "lone surrogate must fail the JSON parse");
        assert_eq!(len, 0);
    }

    #[test]
    fn set_to_list_round_trips_through_boxed_slice_free() {
        let h = mimi_set_new();
        assert_ne!(h, 0);
        unsafe { mimi_set_insert(h, 11) };
        unsafe { mimi_set_insert(h, 22) };
        unsafe { mimi_set_insert(h, 33) };
        let mut len: i64 = -1;
        let ptr = unsafe { mimi_set_to_list(h, &mut len as *mut i64) };
        assert!(!ptr.is_null());
        assert_eq!(len, 3);
        // SAFETY: mimi_set_to_list hands out a boxed slice of `len` handles.
        unsafe {
            let slice = std::slice::from_raw_parts(ptr, len as usize);
            let mut vals: Vec<i64> = slice.to_vec();
            vals.sort_unstable();
            assert_eq!(vals, vec![11, 22, 33]);
        }
        unsafe { mimi_set_list_free(ptr, len) };
        unsafe { mimi_set_destroy(h) };
    }

    // ── Fix #9: fail-loud json accessors (non-aborting paths only) ────

    #[test]
    fn json_accessors_happy_paths_still_work() {
        let obj = b"{\"name\":\"mimi\",\"count\":3,\"pi\":3.5}\0";
        let key_name = b"name\0";
        let key_count = b"count\0";
        let key_missing = b"nope\0";
        // json_get_string returns the value for present keys.
        let s = unsafe { json_get_string(obj.as_ptr() as _, key_name.as_ptr() as _) };
        assert_eq!(owned_str(s), "mimi");
        // json_get_int parses integer values.
        assert_eq!(
            unsafe { json_get_int(obj.as_ptr() as _, key_count.as_ptr() as _) },
            3
        );
        // json_has_key distinguishes present/missing without aborting.
        assert_eq!(
            unsafe { json_has_key(obj.as_ptr() as _, key_name.as_ptr() as _) },
            1
        );
        assert_eq!(
            unsafe { json_has_key(obj.as_ptr() as _, key_missing.as_ptr() as _) },
            0
        );
        // json_is_valid_json must NEVER abort: 1 for valid...
        assert_eq!(unsafe { mimi_is_valid_json(obj.as_ptr() as _) }, 1);
        // ...0 for malformed.
        assert_eq!(unsafe { mimi_is_valid_json(b"{oops\0".as_ptr() as _) }, 0);
    }

    #[test]
    fn json_accessors_reject_unknown_escapes_and_raw_controls() {
        // The permissive accessor path must still reject malformed string
        // tokens (unknown escapes / raw control chars) for serde parity.
        let bad_value_escape = b"{\"a\":\"\\q\"}\0";
        let bad_key_escape = b"{\"\\q\":1}\0";
        let bad_value_control = b"{\"a\":\"\x01\"}\0";
        let bad_key_control = b"{\"\x01\":1}\0";
        assert!(json_get_inner(bad_value_escape.as_ptr() as _, b"a\0".as_ptr() as _).is_err());
        assert!(json_get_inner(bad_key_escape.as_ptr() as _, b"a\0".as_ptr() as _).is_err());
        assert!(json_get_inner(bad_value_control.as_ptr() as _, b"a\0".as_ptr() as _).is_err());
        assert!(json_get_inner(bad_key_control.as_ptr() as _, b"a\0".as_ptr() as _).is_err());
        // Valid escaped controls still parse.
        let ok = b"{\"a\":\"\\n\\t\\\"\"}\0";
        assert!(json_get_inner(ok.as_ptr() as _, b"a\0".as_ptr() as _).is_ok());
    }

    #[test]
    fn json_array_length_and_get_element_happy_paths() {
        let arr = b"[10,\"twenty\",[3]]\0";
        assert_eq!(unsafe { json_array_length(arr.as_ptr() as _) }, 3);
        let e = unsafe { json_get_element(arr.as_ptr() as _, 1) };
        assert_eq!(owned_str(e), "twenty");
    }

    #[test]
    fn json_accessor_parse_error_and_type_errors_abort() {
        // Covered by the central e2e abort harness (process death cannot be
        // asserted in-process). Cases exercised there:
        //   json_get_string("{malformed", "k")  → "json_get_string parse error"
        //   json_get_string("{}", "k")          → "key 'k' not found"
        //   json_get_int("{\"a\":\"x\"}", "a")  → "is not a number"
        //   json_get_int("{\"a\":1.5}", "a")    → "is not an integer"
        //   json_get_element("[1]", 5)          → "index 5 out of bounds"
        //   json_array_length("{\"a\":1}")       → "value is not an array"
        //   json_has_key("{bad", "k")           → "json_has_key parse error"
        //   mimi_inject_fault("S")              → abort (fix #10)
    }
}

// ---------------------------------------------------------------------------
// audit-unfixed-2026-08-05 收尾包 D regression tests
// §10-#27 (ReDoS time budget — tests in runtime/regex.rs),
// §10-#31 (product serializer mincore), §10-#35 (map destroy reclaims).
// ---------------------------------------------------------------------------
#[cfg(test)]
mod audit_pkgd_tests {
    use super::*;

    fn owned_str(ptr: *mut std::ffi::c_char) -> String {
        assert!(!ptr.is_null());
        // SAFETY: alloc_c_string results are NUL-terminated heap strings.
        let s = unsafe { std::ffi::CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned();
        unsafe {
            mimi_string_free(ptr);
        }
        s
    }

    // ── §10-#31: product serializers must mincore-probe, never segfault ──

    #[test]
    fn product_serializer_unmapped_handle_serializes_zeros_not_segfault() {
        // Same unmapped canonical-userspace hole used by audit_wave1_tests.
        let garbage: ValueHandle = 0x0000_7000_0000_0000;
        let h = mimi_map_new();
        let key = b"k\0".as_ptr() as *const std::ffi::c_char;
        unsafe {
            mimi_map_set(h, key, garbage);
        }
        // Pre-fix this dereferenced the handle blind (SIGSEGV).
        let out = owned_str(unsafe { mimi_map_to_json_product_i64(h, 2, 0) });
        assert_eq!(out, "{\"k\":[0,0]}");
        unsafe { mimi_map_destroy(h) };
    }

    #[test]
    fn product_serializer_small_handle_serializes_zeros_not_segfault() {
        // Below MIN_HEAP: provably not a heap pointer.
        let h = mimi_map_new();
        let key = b"k\0".as_ptr() as *const std::ffi::c_char;
        unsafe {
            mimi_map_set(h, key, 8);
        }
        let out = owned_str(unsafe { mimi_map_to_json_product_i64(h, 2, 1) });
        assert_eq!(out, "{\"k\":(0, 0)}");
        unsafe { mimi_map_destroy(h) };
    }

    #[test]
    fn product_serializer_real_packs_still_round_trip() {
        // The probe must not perturb legitimate heap-packed values.
        let json = b"{\"a\":[1,2],\"b\":[3,4]}\0";
        let h = unsafe { mimi_map_from_json_product_i64(json.as_ptr() as _, 2) };
        assert_ne!(h, 0);
        let out = owned_str(unsafe { mimi_map_to_json_product_i64(h, 2, 0) });
        assert_eq!(out, "{\"a\":[1,2],\"b\":[3,4]}");
        unsafe { mimi_map_destroy(h) };
    }

    #[test]
    fn composite_serializers_unmapped_handle_fail_closed() {
        // P1-21: every map composite serializer must mincore-probe composite
        // packs before dereferencing them. A wild unmapped handle used to
        // SIGSEGV in result/option/map composite paths; it must now serialize
        // as null without reading memory.
        type JsonFn = unsafe extern "C" fn(MapHandle, i64, i64) -> *mut std::ffi::c_char;
        let cases: &[JsonFn] = &[
            mimi_map_to_json_result_product_i64,
            mimi_map_to_json_result_map_product_i64,
            mimi_map_to_json_option_map_list_product_i64,
            mimi_map_to_json_result_option_list_product_i64,
            mimi_map_to_json_result_set_product_i64,
        ];
        let garbage: ValueHandle = 0x0000_7000_0000_0000;
        for &f in cases {
            let h = mimi_map_new();
            let key = b"k\0".as_ptr() as *const std::ffi::c_char;
            unsafe {
                mimi_map_set(h, key, garbage);
            }
            let out = owned_str(unsafe { f(h, 2, 0) });
            assert_eq!(
                out, "{\"k\":null}",
                "fail-closed output for composite serializer"
            );
            unsafe { mimi_map_destroy(h) };
        }
    }

    // ── §10-#35: destroy() must reclaim map-owned value buffers ────────
    // Registration is asserted EXACTLY via the per-map owned count (race-free).
    // Reclamation is asserted on the global balance with retries: concurrent
    // tests (audit_fix_runtime_core) allocate/free through the same builders,
    // so a single sample can be transiently skewed; a real leak pins the
    // buffers forever and fails every attempt.

    fn attempt_reclaim_check(json: &[u8], arity: i64, list: bool) -> bool {
        let baseline = mimi_runtime_map_owned_value_balance();
        let h = if list {
            unsafe { mimi_map_from_json_list_product_i64(json.as_ptr() as _, arity) }
        } else {
            unsafe { mimi_map_from_json_product_i64(json.as_ptr() as _, arity) }
        };
        if h == 0 {
            return false;
        }
        // Race-free exact registration proof.
        let registered = mimi_map_owned_value_count(h) == 2;
        unsafe { mimi_map_destroy(h) };
        registered && mimi_runtime_map_owned_value_balance() <= baseline
    }

    #[test]
    fn map_destroy_reclaims_owned_product_packs() {
        let json = b"{\"a\":[1,2],\"b\":[3,4]}\0";
        let ok = (0..5).any(|_| attempt_reclaim_check(json, 2, false));
        assert!(
            ok,
            "destroy must free every owned pack (leak pins the balance)"
        );
    }

    #[test]
    fn map_destroy_reclaims_owned_list_of_packs() {
        let json = b"{\"a\":[[1,2],[3,4]],\"b\":[[5,6]]}\0";
        // Serialization must still work while live (frees nothing).
        let h = unsafe { mimi_map_from_json_list_product_i64(json.as_ptr() as _, 2) };
        assert_ne!(h, 0);
        assert_eq!(mimi_map_owned_value_count(h), 2);
        let out = owned_str(unsafe { mimi_map_to_json_list_product_i64(h, 2, 0) });
        assert_eq!(out, "{\"a\":[[1,2],[3,4]],\"b\":[[5,6]]}");
        assert_eq!(mimi_map_owned_value_count(h), 2, "serialize must not free");
        unsafe { mimi_map_destroy(h) };
        let ok = (0..5).any(|_| attempt_reclaim_check(json, 2, true));
        assert!(
            ok,
            "destroy must free headers + data arrays + element packs"
        );
    }

    // ── 0.35.28 H5: nested from_json builders must register their own      ──
    // shell (tagged pack / Box list) so destroy() reclaims it. Inner object
    // handles (map/set/list) stay bounded leaks by design — a caller may
    // hold them via map_get; see mimi_map_destroy comment. Errored Result
    // messages are builder-owned C strings and are reclaimed.

    fn nested_reclaim_check(build: impl Fn() -> MapHandle, entries: i64) -> bool {
        let h = build();
        if h == 0 {
            return false;
        }
        // Race-free exact registration proof: one registered shell per entry.
        let registered = mimi_map_owned_value_count(h) == entries;
        let after_build = mimi_runtime_map_owned_value_balance();
        unsafe { mimi_map_destroy(h) };
        let after_destroy = mimi_runtime_map_owned_value_balance();
        // destroy() must reclaim exactly the shells this map registered.
        // Inner map/set/list handles stay as bounded leaks by design (a
        // caller may hold them via map_get), so they are excluded from the
        // balance delta; their own shells are reclaimed when *they* die.
        registered && after_destroy == after_build - entries
    }

    #[test]
    fn map_destroy_reclaims_nested_option_pack_shells() {
        // Map of Option of Map of product: None = null, Some = inner map.
        let json = b"{\"a\":null,\"b\":{\"x\":[3,4]},\"c\":{\"y\":[5,6]}}\0";
        let ok = (0..5).any(|_| {
            nested_reclaim_check(
                || unsafe { mimi_map_from_json_option_map_product_i64(json.as_ptr() as _, 2) },
                3,
            )
        });
        assert!(ok, "option pack shells must be registered + reclaimed");
        // Serialization round-trips while live.
        let h = unsafe { mimi_map_from_json_option_map_product_i64(json.as_ptr() as _, 2) };
        assert_ne!(h, 0);
        let out = owned_str(unsafe { mimi_map_to_json_option_map_product_i64(h, 2, 0) });
        assert_eq!(
            out,
            "{\"a\":\"None\",\"b\":{\"Some\":[{\"x\":[3,4]}]},\"c\":{\"Some\":[{\"y\":[5,6]}]}}"
        );
        unsafe { mimi_map_destroy(h) };
    }

    #[test]
    fn map_destroy_reclaims_nested_result_pack_shells_and_err_messages() {
        // Map of Result of Map of product: Ok = {"Ok":{...}}, Err = {"Err":"msg"}
        // — the Err message is a builder-owned C string freed on destroy.
        let json = b"{\"a\":{\"Err\":\"boom\"},\"b\":{\"Ok\":{\"x\":[7,8]}}}\0";
        let ok = (0..5).any(|_| {
            nested_reclaim_check(
                || unsafe { mimi_map_from_json_result_map_product_i64(json.as_ptr() as _, 2) },
                2,
            )
        });
        assert!(ok, "result pack shells must be registered + reclaimed");
        let h = unsafe { mimi_map_from_json_result_map_product_i64(json.as_ptr() as _, 2) };
        assert_ne!(h, 0);
        let out = owned_str(unsafe { mimi_map_to_json_result_map_product_i64(h, 2, 0) });
        assert_eq!(
            out,
            "{\"a\":{\"Err\":[\"boom\"]},\"b\":{\"Ok\":[{\"x\":[7,8]}]}}"
        );
        unsafe { mimi_map_destroy(h) };
    }

    #[test]
    fn map_destroy_reclaims_nested_list_object() {
        // Map of List of Option of product: the list is a Box-allocated
        // MimiList from mimi_list_from_json_* — destroy must mimi_list_free
        // it (struct + data array + Record element pack bases).
        let json = b"{\"a\":[null,[1,2]],\"b\":[[3,4],null]}\0";
        let ok = (0..5).any(|_| {
            nested_reclaim_check(
                || unsafe { mimi_map_from_json_list_option_product_i64(json.as_ptr() as _, 2) },
                2,
            )
        });
        assert!(ok, "Box MimiList shells must be registered + reclaimed");
        let h = unsafe { mimi_map_from_json_list_option_product_i64(json.as_ptr() as _, 2) };
        assert_ne!(h, 0);
        assert_eq!(mimi_map_owned_value_count(h), 2);
        let out = owned_str(unsafe { mimi_map_to_json_list_option_product_i64(h, 2, 0) });
        assert_eq!(
            out,
            "{\"a\":[\"None\",{\"Some\":[[1,2]]}],\"b\":[{\"Some\":[[3,4]]},\"None\"]}"
        );
        assert_eq!(mimi_map_owned_value_count(h), 2, "serialize must not free");
        unsafe { mimi_map_destroy(h) };
    }

    #[test]
    fn map_destroy_reclaims_deeply_nested_result_option_list() {
        // Result of Option of List of product — exercises the PackErrCString
        // shell (Ok(None) + Err branches; inner list header stays bounded).
        let json = b"{\"a\":{\"Ok\":null},\"b\":{\"Err\":\"nope\"}}\0";
        let ok = (0..5).any(|_| {
            nested_reclaim_check(
                || unsafe {
                    mimi_map_from_json_result_option_list_product_i64(json.as_ptr() as _, 2)
                },
                2,
            )
        });
        assert!(ok, "deeply nested result shells must be reclaimed");
        let h = unsafe { mimi_map_from_json_result_option_list_product_i64(json.as_ptr() as _, 2) };
        assert_ne!(h, 0);
        let out = owned_str(unsafe { mimi_map_to_json_result_option_list_product_i64(h, 2, 0) });
        assert_eq!(
            out,
            "{\"a\":{\"Ok\":[\"None\"]},\"b\":{\"Err\":[\"nope\"]}}"
        );
        unsafe { mimi_map_destroy(h) };
    }

    // ── M7 (0.35.37): mimi_map_from_list silent truncation / bad-key skip ──

    /// The 1M cap must truncate loudly (warn to stderr) and the returned map
    /// must contain exactly the capped count — not silently lose entries.
    #[test]
    fn map_from_list_cap_truncates_loudly() {
        // Allocate 1M+64 key/value handles. Keys must be *valid mapped C
        // strings* or safe_c_string_from_handle rejects them; use
        // alloc_c_string for real heap strings.
        let n = 1_000_000 + 64i64;
        let mut keys: Vec<ValueHandle> = Vec::with_capacity(n as usize);
        let mut values: Vec<ValueHandle> = Vec::with_capacity(n as usize);
        for i in 0..n {
            let k = format!("k{}", i);
            let c = alloc_c_string(&k);
            keys.push(c as ValueHandle);
            values.push(i as ValueHandle);
        }
        let h = unsafe { mimi_map_from_list(keys.as_mut_ptr(), values.as_mut_ptr(), n) };
        // Loud truncation: only the first 1M entries survive.
        assert_eq!(unsafe { mimi_map_size(h) }, 1_000_000);
        unsafe { mimi_map_destroy(h) };
        // Free the key strings (map copies them; we own the originals).
        for &k in &keys {
            unsafe { mimi_string_free(k as *mut std::ffi::c_char) };
        }
    }

    /// A wild/foreign key handle must be skipped (with a warning), never
    /// inserted under a garbage key; the remaining valid pairs still land.
    #[test]
    fn map_from_list_bad_key_skipped_others_inserted() {
        let k1 = alloc_c_string("alpha");
        let k2 = alloc_c_string("beta");
        let mut keys = vec![k1 as ValueHandle, 0x0000_7000_0000_0000, k2 as ValueHandle];
        let mut values = vec![1usize as ValueHandle, 2, 3];
        let h = unsafe { mimi_map_from_list(keys.as_mut_ptr(), values.as_mut_ptr(), 3) };
        assert_eq!(
            unsafe { mimi_map_size(h) },
            2,
            "garbage key must be skipped, valid keys kept"
        );
        // "beta" -> 3 survived (third pair). "alpha" -> 1 survived (first).
        let out = owned_str(unsafe { mimi_map_to_json_product_i64(h, 2, 0) });
        assert!(out.contains("\"alpha\""));
        assert!(out.contains("\"beta\""));
        assert!(!out.contains("garbage"));
        unsafe { mimi_map_destroy(h) };
        unsafe {
            mimi_string_free(k1 as *mut std::ffi::c_char);
            mimi_string_free(k2 as *mut std::ffi::c_char);
        }
    }

    /// Sanity: normal path with mixed integer/pointer values is unaffected —
    /// values must never be validated as heap pointers (integers are legal).
    #[test]
    fn map_from_list_mixed_values_unaffected() {
        let k1 = alloc_c_string("int");
        let k2 = alloc_c_string("zero");
        // Integer values 7 and 0 (0 is below MIN_HEAP — must still insert).
        let mut keys = vec![k1 as ValueHandle, k2 as ValueHandle];
        let mut values = vec![7usize as ValueHandle, 0usize as ValueHandle];
        let h = unsafe { mimi_map_from_list(keys.as_mut_ptr(), values.as_mut_ptr(), 2) };
        assert_eq!(unsafe { mimi_map_size(h) }, 2);
        let out = owned_str(unsafe { mimi_map_to_json_product_i64(h, 2, 0) });
        assert!(out.contains("\"int\":"));
        assert!(out.contains("\"zero\":"));
        unsafe { mimi_map_destroy(h) };
        unsafe {
            mimi_string_free(k1 as *mut std::ffi::c_char);
            mimi_string_free(k2 as *mut std::ffi::c_char);
        }
    }
}

#[cfg(test)]
mod runtime_ptr_readable_tests {
    use super::*;

    #[test]
    fn ptr_readable_rejects_absurd_len_without_scanning() {
        let arr = [0u8; 8];
        unsafe {
            // Normal small mapped stack span is readable.
            assert_eq!(mimi_runtime_ptr_readable(arr.as_ptr(), 8), 1);
            // A >1MiB len is rejected up front even though the base pointer
            // is mapped, preventing an unbounded page-mincore loop (P2-5).
            assert_eq!(mimi_runtime_ptr_readable(arr.as_ptr(), (1 << 20) + 1), 0);
            assert_eq!(mimi_runtime_ptr_readable(arr.as_ptr(), i64::MAX), 0);
        }
    }

    #[test]
    fn list_push_rejects_negative_len() {
        unsafe {
            let mut list = MimiList {
                len: -1,
                data: std::ptr::null_mut(),
                owns_data: false,
                element_kind: ListElementKind::I64,
                has_header: false,
                string_abi: 0,
            };
            mimi_list_push_i64(&mut list, 42);
            mimi_list_push_f64(&mut list, 1.5);
            mimi_list_push_string(&mut list, b"x\0".as_ptr() as *const std::ffi::c_char);
            assert_eq!(list.len, -1, "negative list length must be left untouched");
            let grown = mimi_list_push_grow(&mut list, 4);
            assert!(
                grown.is_null(),
                "push_grow must reject negative list length"
            );
        }
    }
}
