//! Mimi runtime filesystem + process operations — directory/path helpers,
//! `exec`/pipe, file stat, append, and environment variables.
//!
//! Extracted verbatim from `runtime/mod.rs` (the `Directory & path` and
//! `Process & advanced file` sections) during the 0.1.0 mechanical split
//! (behavior bit-exact). Pure `extern "C"` leaf: no crate-level Rust-path
//! callers. Forward deps on the parent module's `alloc_c_string` /
//! `cstr_to_string` helpers and the shared `MimiList` type.

use std::ffi::CStr;

#[cfg(standalone)]
use super::libc;
use super::{alloc_c_string, cstr_to_string, ListElementKind, MimiList};

/// Maximum recursion depth for `mimi_walk_dir`. Prevents unbounded stack
/// growth on pathological directory trees (batch4/06 P2).
const MAX_WALK_DEPTH: usize = 64;
/// Maximum number of paths returned by `mimi_walk_dir`. Prevents a hostile
/// filesystem from exhausting memory through an enormous file count.
const MAX_WALK_RESULTS: usize = 1_000_000;

// ─── Directory & path operations ───────────────────────────────

/// Free element pointers previously produced by `alloc_c_string`.
fn free_c_string_ptrs(items: &[*mut std::ffi::c_char]) {
    for &p in items {
        if !p.is_null() {
            // SAFETY: each non-null pointer was allocated by `alloc_c_string`
            // (mimi_alloc) and is freed exactly once here through the
            // matching deallocator (audit 2026-08-05, N-1 pairing).
            super::mimi_free(p as *mut std::ffi::c_void);
        }
    }
}

/// H1-pattern fix (matches `mimi_str_split` in mod.rs): move
/// `alloc_c_string` element pointers out of a `Vec` into a fresh
/// `libc::malloc` array so `mimi_list_free` can free the data buffer with
/// `libc::free` (same allocator). Audit 2026-08-05 (H-26): lists built from
/// this array carry `has_header=false`, so `list_cap`/`mimi_list_free` never
/// read `data[-8]` — the explicit flag replaced the old negative-value
/// heuristic that made every such free an out-of-bounds read. The Vec's own
/// backing storage is dropped normally (allocator-consistent). Returns null
/// when the input is empty or on allocation failure (elements are freed in
/// the failure case so they do not leak).
fn malloc_c_string_array(items: Vec<*mut std::ffi::c_char>) -> *mut *mut std::ffi::c_char {
    if items.is_empty() {
        return std::ptr::null_mut();
    }
    let data_size = match items
        .len()
        .checked_mul(std::mem::size_of::<*mut std::ffi::c_char>())
    {
        Some(s) => s,
        None => {
            free_c_string_ptrs(&items);
            return std::ptr::null_mut();
        }
    };
    // SAFETY: data_size > 0 (items non-empty); result is checked for null.
    let ptr = unsafe { libc::malloc(data_size) as *mut *mut std::ffi::c_char };
    if ptr.is_null() {
        free_c_string_ptrs(&items);
        return std::ptr::null_mut();
    }
    for (i, p) in items.iter().enumerate() {
        // SAFETY: i < items.len() and `ptr` is a fresh allocation of
        // items.len() pointer slots; each slot is written exactly once.
        unsafe {
            *ptr.add(i) = *p;
        }
    }
    ptr
}

/// Returns a Mimi List of entry names in the given directory.
/// Returns an empty list on error (not a directory, permission denied, etc.).
///
/// # Safety
/// Pointer arguments must be valid for the C ABI contract:
/// C strings must be NUL-terminated (unless documented otherwise),
/// result/free pointers must come from matching mimi runtime calls,
/// and lists/stat handles must be live values created by this runtime.
#[no_mangle]
pub unsafe extern "C" fn mimi_listdir(path: *const std::ffi::c_char) -> *mut MimiList {
    let path_str = if path.is_null() {
        return Box::into_raw(Box::new(MimiList::new_with_kind(ListElementKind::String)));
    } else {
        // SAFETY: `path` was checked non-null above.
        match unsafe { CStr::from_ptr(path) }.to_str() {
            Ok(s) => s,
            Err(_) => {
                return Box::into_raw(Box::new(MimiList::new_with_kind(ListElementKind::String)))
            }
        }
    };
    let entries: Vec<*mut std::ffi::c_char> = match std::fs::read_dir(path_str) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().to_str().map(alloc_c_string))
            .collect(),
        Err(_) => return Box::into_raw(Box::new(MimiList::new_with_kind(ListElementKind::String))),
    };
    let len = entries.len() as i64;
    // Audit fix (H1 pattern): copy the element pointers out of the Vec into a
    // libc::malloc'd array. The old code handed the Vec buffer to MimiList
    // with owns_data=true → list_cap read data[-8] OOB and mimi_list_free
    // freed a Vec buffer via libc::free (allocator mismatch). Since the
    // 2026-08-05 audit (H-26) the list also carries has_header=false, so
    // data[-8] is never read regardless of allocator.
    let data_ptr = malloc_c_string_array(entries);
    if data_ptr.is_null() && len > 0 {
        // Allocation failure (elements already freed by the helper): degrade
        // to an empty list, never a null list pointer (callers do not
        // null-check MimiList*).
        return Box::into_raw(Box::new(MimiList::new_with_kind(ListElementKind::String)));
    }
    // 0.31.23: directory entries are strings. The MimiList STRUCT is
    // Box-allocated, matching mimi_list_free which frees it via
    // Box::from_raw (mod.rs). No hidden capacity header: has_header=false
    // (with_data default) → free(data) is direct and data[-8] is never read.
    Box::into_raw(Box::new(MimiList::with_data(
        data_ptr,
        len,
        true,
        ListElementKind::String,
    )))
}

/// Returns 1 if path is a directory, 0 otherwise.
///
/// # Safety
/// Pointer arguments must be valid for the C ABI contract:
/// C strings must be NUL-terminated (unless documented otherwise),
/// result/free pointers must come from matching mimi runtime calls,
/// and lists/stat handles must be live values created by this runtime.
#[no_mangle]
pub unsafe extern "C" fn mimi_is_dir(path: *const std::ffi::c_char) -> i64 {
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
///
/// # Safety
/// Pointer arguments must be valid for the C ABI contract:
/// C strings must be NUL-terminated (unless documented otherwise),
/// result/free pointers must come from matching mimi runtime calls,
/// and lists/stat handles must be live values created by this runtime.
#[no_mangle]
pub unsafe extern "C" fn mimi_is_file(path: *const std::ffi::c_char) -> i64 {
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
///
/// # Safety
/// Pointer arguments must be valid for the C ABI contract:
/// C strings must be NUL-terminated (unless documented otherwise),
/// result/free pointers must come from matching mimi runtime calls,
/// and lists/stat handles must be live values created by this runtime.
#[no_mangle]
pub unsafe extern "C" fn mimi_path_join(
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
///
/// # Safety
/// Pointer arguments must be valid for the C ABI contract:
/// C strings must be NUL-terminated (unless documented otherwise),
/// result/free pointers must come from matching mimi runtime calls,
/// and lists/stat handles must be live values created by this runtime.
#[no_mangle]
pub unsafe extern "C" fn mimi_path_ext(path: *const std::ffi::c_char) -> *mut std::ffi::c_char {
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
///
/// # Safety
/// Pointer arguments must be valid for the C ABI contract:
/// C strings must be NUL-terminated (unless documented otherwise),
/// result/free pointers must come from matching mimi runtime calls,
/// and lists/stat handles must be live values created by this runtime.
#[no_mangle]
pub unsafe extern "C" fn mimi_path_basename(
    path: *const std::ffi::c_char,
) -> *mut std::ffi::c_char {
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
///
/// # Safety
/// Pointer arguments must be valid for the C ABI contract:
/// C strings must be NUL-terminated (unless documented otherwise),
/// result/free pointers must come from matching mimi runtime calls,
/// and lists/stat handles must be live values created by this runtime.
#[no_mangle]
pub unsafe extern "C" fn mimi_path_dirname(path: *const std::ffi::c_char) -> *mut std::ffi::c_char {
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
///
/// # Safety
/// Pointer arguments must be valid for the C ABI contract:
/// C strings must be NUL-terminated (unless documented otherwise),
/// result/free pointers must come from matching mimi runtime calls,
/// and lists/stat handles must be live values created by this runtime.
#[no_mangle]
pub unsafe extern "C" fn mimi_walk_dir(path: *const std::ffi::c_char) -> *mut MimiList {
    let empty = || Box::into_raw(Box::new(MimiList::new_with_kind(ListElementKind::String)));
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
    let mut visited = std::collections::HashSet::new();
    walk_dir_recursive(path_str, &mut results, 0, &mut visited);
    let len = results.len() as i64;
    let items: Vec<*mut std::ffi::c_char> =
        results.into_iter().map(|s| alloc_c_string(&s)).collect();
    // Audit fix (H1 pattern): same as mimi_listdir — copy the element
    // pointers out of the Vec into a libc::malloc'd array so mimi_list_free
    // frees with the matching allocator; has_header=false (H-26) keeps
    // list_cap from ever reading data[-8].
    let data_ptr = malloc_c_string_array(items);
    if data_ptr.is_null() && len > 0 {
        // Allocation failure (elements already freed by the helper): degrade
        // to an empty list, never a null list pointer.
        return empty();
    }
    // 0.31.23: file paths are strings. The MimiList STRUCT is Box-allocated,
    // matching mimi_list_free which frees it via Box::from_raw (mod.rs).
    Box::into_raw(Box::new(MimiList::with_data(
        data_ptr,
        len,
        true,
        ListElementKind::String,
    )))
}

fn walk_dir_recursive(
    dir: &str,
    results: &mut Vec<String>,
    depth: usize,
    visited: &mut std::collections::HashSet<std::path::PathBuf>,
) {
    if depth > MAX_WALK_DEPTH || results.len() >= MAX_WALK_RESULTS {
        return;
    }
    // Canonicalize each directory before recursing so symlink cycles cannot
    // make the walk loop forever (batch4/06 P2).
    let Ok(canon) = std::fs::canonicalize(dir) else {
        return;
    };
    if !visited.insert(canon) {
        return;
    }
    let rd = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return,
    };
    for entry in rd.flatten() {
        if results.len() >= MAX_WALK_RESULTS {
            break;
        }
        let path = entry.path();
        let path_str = path.to_string_lossy().into_owned();
        if path.is_dir() {
            walk_dir_recursive(&path_str, results, depth + 1, visited);
        } else {
            results.push(path_str);
        }
    }
}

/// Creates a directory and all parent directories. Returns 1 on success, 0 on failure.
///
/// # Safety
/// Pointer arguments must be valid for the C ABI contract:
/// C strings must be NUL-terminated (unless documented otherwise),
/// result/free pointers must come from matching mimi runtime calls,
/// and lists/stat handles must be live values created by this runtime.
#[no_mangle]
pub unsafe extern "C" fn mimi_mkdir_p(path: *const std::ffi::c_char) -> i64 {
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
///
/// # Safety
/// Pointer arguments must be valid for the C ABI contract:
/// C strings must be NUL-terminated (unless documented otherwise),
/// result/free pointers must come from matching mimi runtime calls,
/// and lists/stat handles must be live values created by this runtime.
#[no_mangle]
pub unsafe extern "C" fn mimi_remove_file(path: *const std::ffi::c_char) -> i64 {
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

/// Max bytes captured per output stream by the exec family (10 MB, both
/// backends — VM builtins reuse this same helper, so the cap is shared).
pub(crate) const MAX_EXEC_OUTPUT: usize = 10 * 1024 * 1024;

/// Spawn `cmd`, capture stdout/stderr capped at `MAX_EXEC_OUTPUT` per stream,
/// and **keep draining** the pipes past the cap so the child never blocks on
/// a full pipe buffer (0.35.29 H12). The old `Command::output()` collected
/// the full output and only truncated afterwards — a child writing >16 MB
/// OOM'd the interpreter/compiled process. Return (stdout, stderr, code).
///
/// This is shared with the bytecode VM builtins (`builtin_exec*` in
/// misc.rs) so both backends cap identically — L1 by construction.
pub(crate) fn run_exec_capped(cmd: &mut std::process::Command) -> (Vec<u8>, Vec<u8>, Option<i32>) {
    use std::io::Read;
    use std::process::{ChildStdout, Stdio};

    fn drain_capped(mut r: impl Read) -> Vec<u8> {
        let mut kept = Vec::with_capacity(MAX_EXEC_OUTPUT.min(65536));
        let mut buf = [0u8; 16384];
        loop {
            match r.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let room = MAX_EXEC_OUTPUT - kept.len();
                    if room > 0 {
                        let take = room.min(n);
                        kept.extend_from_slice(&buf[..take]);
                    }
                    // Bytes past the cap are read and discarded — the child
                    // must never block on a full pipe buffer.
                }
                Err(_) => break,
            }
        }
        kept
    }

    // SAFETY: stdout/stderr are captured into pipes owned by us; the child
    // is waited on below. Both streams are read concurrently so a child
    // writing a lot to one stream cannot block because the other is full.
    let mut child = match cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() {
        Ok(c) => c,
        Err(_) => return (Vec::new(), Vec::new(), None),
    };
    let out_pipe: Option<ChildStdout> = child.stdout.take();
    let err_pipe = child.stderr.take();
    let (out_pipe, err_pipe) = match (out_pipe, err_pipe) {
        (Some(o), Some(e)) => (o, e),
        _ => {
            let _ = child.wait();
            return (Vec::new(), Vec::new(), None);
        }
    };
    let stderr_t = std::thread::spawn(move || drain_capped(err_pipe));
    let stdout = drain_capped(out_pipe);
    let stderr = stderr_t.join().unwrap_or_default();
    let code = child.wait().ok().and_then(|s| s.code());
    (stdout, stderr, code)
}

/// Executes a shell command via `sh -c`. Returns a heap-allocated MimiExecResult.
/// Uses shell interpretation (pipelines, variables, redirections).
///
/// Security note (HIGH): `cmd` is passed directly to `sh -c`. If `cmd`
/// contains user-controlled input, shell injection is possible. Only
/// use `mimi_exec` with trusted, hard-coded command strings. For
/// untrusted input, use `mimi_exec_safe` which avoids the shell.
/// Caller must free with `mimi_exec_free`.
///
/// # Safety
/// Pointer arguments must be valid for the C ABI contract:
/// C strings must be NUL-terminated (unless documented otherwise),
/// result/free pointers must come from matching mimi runtime calls,
/// and lists/stat handles must be live values created by this runtime.
#[no_mangle]
pub unsafe extern "C" fn mimi_exec(cmd: *const std::ffi::c_char) -> *mut MimiExecResult {
    // RT-H5: optional hard refuse under MIMI_EXEC_STRICT / MIMI_FFI_STRICT.
    // Read the strictness flags under SETENV_LOCK: setenv may reallocate the
    // global environ while mimi_set_env is writing.
    let strict = {
        let _lock = SETENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::var("MIMI_EXEC_STRICT")
            .or_else(|_| std::env::var("MIMI_FFI_STRICT"))
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    };
    if strict {
        let res = Box::new(MimiExecResult {
            exit_code: -1,
            stdout: alloc_c_string(""),
            stderr: alloc_c_string(
                "exec error: mimi_exec refused under MIMI_EXEC_STRICT/MIMI_FFI_STRICT; use mimi_exec_safe",
            ),
        });
        return Box::into_raw(res);
    }
    static EXEC_WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
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
    let (stdout_bytes, stderr_bytes, code) =
        run_exec_capped(std::process::Command::new("sh").arg("-c").arg(cmd_str));
    let stdout = String::from_utf8_lossy(&stdout_bytes).to_string();
    let stderr = String::from_utf8_lossy(&stderr_bytes).to_string();
    let exit_code = code.unwrap_or(-1);
    let res = Box::new(MimiExecResult {
        exit_code: exit_code as i64,
        stdout: alloc_c_string(&stdout),
        stderr: alloc_c_string(&stderr),
    });
    Box::into_raw(res)
}

/// Frees a MimiExecResult allocated by mimi_exec.
/// stdout/stderr are allocated by alloc_c_string (mimi_alloc), so they are
/// freed through mimi_free — the matching deallocator under both normal and
/// miri builds (audit 2026-08-05, N-1).
///
/// # Safety
/// Pointer arguments must be valid for the C ABI contract:
/// C strings must be NUL-terminated (unless documented otherwise),
/// result/free pointers must come from matching mimi runtime calls,
/// and lists/stat handles must be live values created by this runtime.
#[no_mangle]
pub unsafe extern "C" fn mimi_exec_free(res: *mut MimiExecResult) {
    if res.is_null() {
        return;
    }
    // SAFETY: `res` was checked non-null; stdout/stderr were allocated by `alloc_c_string`.
    unsafe {
        let r = Box::from_raw(res);
        if !r.stdout.is_null() {
            super::mimi_free(r.stdout as *mut std::ffi::c_void);
        }
        if !r.stderr.is_null() {
            super::mimi_free(r.stderr as *mut std::ffi::c_void);
        }
    }
}

/// Frees only the MimiExecResult struct, NOT the stdout/stderr strings.
/// Used by codegen after extracting string pointers into ExecResult struct.
///
/// # Safety
/// Pointer arguments must be valid for the C ABI contract:
/// C strings must be NUL-terminated (unless documented otherwise),
/// result/free pointers must come from matching mimi runtime calls,
/// and lists/stat handles must be live values created by this runtime.
#[no_mangle]
pub unsafe extern "C" fn mimi_exec_free_struct(res: *mut MimiExecResult) {
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
///
/// # Safety
/// Pointer arguments must be valid for the C ABI contract:
/// C strings must be NUL-terminated (unless documented otherwise),
/// result/free pointers must come from matching mimi runtime calls,
/// and lists/stat handles must be live values created by this runtime.
#[no_mangle]
pub unsafe extern "C" fn mimi_exec_pipe(cmd: *const std::ffi::c_char) -> *mut std::ffi::c_char {
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
    let (stdout_bytes, _stderr, _code) =
        run_exec_capped(std::process::Command::new("sh").arg("-c").arg(cmd_str));
    let stdout = String::from_utf8_lossy(&stdout_bytes).to_string();
    alloc_c_string(&stdout)
}

/// Execute a single program without shell interpretation.
/// `prog` is the program path, `args` are the arguments (excluding argv[0]).
/// Returns a `MimiExecResult` struct. Caller must free with `mimi_exec_free`.
/// No shell injection risk: the program is executed directly via `execvp`.
///
/// # Safety
/// Pointer arguments must be valid for the C ABI contract:
/// C strings must be NUL-terminated (unless documented otherwise),
/// result/free pointers must come from matching mimi runtime calls,
/// and lists/stat handles must be live values created by this runtime.
#[no_mangle]
pub unsafe extern "C" fn mimi_exec_safe(
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
        let mut cmd = std::process::Command::new(&prog_str);
        let (stdout, stderr, code) = run_exec_capped(&mut cmd);
        let exit_code = code.unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&stdout).into_owned();
        let stderr = String::from_utf8_lossy(&stderr).into_owned();
        return Box::into_raw(Box::new(MimiExecResult {
            exit_code: exit_code as i64,
            stdout: alloc_c_string(&stdout),
            stderr: alloc_c_string(&stderr),
        }));
    }
    // SAFETY: args was checked non-null above.
    let lst = unsafe { &*args };
    // batch4/06 P2: validate the list prefix before dereferencing data. A
    // malformed list (negative len, or null data with non-zero len) must not
    // be read as a C string array. Note: codegen passes a two-field {len,
    // data} list through the MimiListAbiPrefix ABI, so element_kind is not
    // part of that contract and cannot be inspected here.
    if lst.len < 0 || (lst.len > 0 && lst.data.is_null()) {
        let res = Box::new(MimiExecResult {
            exit_code: -1,
            stdout: alloc_c_string(""),
            stderr: alloc_c_string("exec_safe error: invalid args list"),
        });
        return Box::into_raw(res);
    }
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
    let (stdout_bytes, stderr_bytes, code) = run_exec_capped(&mut cmd);
    let stdout = String::from_utf8_lossy(&stdout_bytes).to_string();
    let stderr = String::from_utf8_lossy(&stderr_bytes).to_string();
    let exit_code = code.unwrap_or(-1);
    let res = Box::new(MimiExecResult {
        exit_code: exit_code as i64,
        stdout: alloc_c_string(&stdout),
        stderr: alloc_c_string(&stderr),
    });
    Box::into_raw(res)
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
///
/// # Safety
/// Pointer arguments must be valid for the C ABI contract:
/// C strings must be NUL-terminated (unless documented otherwise),
/// result/free pointers must come from matching mimi runtime calls,
/// and lists/stat handles must be live values created by this runtime.
#[no_mangle]
pub unsafe extern "C" fn mimi_file_stat_free(res: *mut MimiStatResult) {
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
///
/// # Safety
/// Pointer arguments must be valid for the C ABI contract:
/// C strings must be NUL-terminated (unless documented otherwise),
/// result/free pointers must come from matching mimi runtime calls,
/// and lists/stat handles must be live values created by this runtime.
#[no_mangle]
pub unsafe extern "C" fn mimi_file_stat(
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

/// Appends content to a file with an explicit byte length. Returns 1 on
/// success, 0 on failure. Length-aware variant used by codegen so embedded
/// NUL bytes are appended intact.
///
/// # Safety
/// Pointer arguments must be valid for the C ABI contract:
/// C strings must be NUL-terminated (unless documented otherwise),
/// `content` must be valid for `len` bytes, and result/free pointers must
/// come from matching mimi runtime calls.
#[no_mangle]
pub unsafe extern "C" fn mimi_append_file_ll(
    path: *const std::ffi::c_char,
    content: *const std::ffi::c_char,
    len: i64,
) -> i64 {
    if path.is_null() || content.is_null() || len < 0 {
        return 0;
    }
    // SAFETY: `path` was checked non-null above.
    let path_str = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(s) => s,
        Err(_) => return 0,
    };
    // SAFETY: `content` is non-null and the caller guarantees it is valid for
    // `len` bytes; the length is checked non-negative above.
    let content_bytes = unsafe { std::slice::from_raw_parts(content as *const u8, len as usize) };
    use std::io::Write;
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path_str)
    {
        Ok(mut file) => {
            if file.write_all(content_bytes).is_ok() {
                1
            } else {
                0
            }
        }
        Err(_) => 0,
    }
}

/// Appends content to a file. Returns 1 on success, 0 on failure.
///
/// # Safety
/// Pointer arguments must be valid for the C ABI contract:
/// C strings must be NUL-terminated (unless documented otherwise),
/// result/free pointers must come from matching mimi runtime calls,
/// and lists/stat handles must be live values created by this runtime.
#[no_mangle]
pub unsafe extern "C" fn mimi_append_file(
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

/// H14 fix: Global mutex serializing ALL runtime access to the process
/// environment — writers (mimi_set_env) AND readers (env.rs `mimi_getenv`).
/// POSIX setenv may reallocate the `environ` array; a concurrent getenv
/// then reads through the old array (use-after-free). The old code locked
/// writers only, which prevented writer/writer races but NOT the
/// writer/reader realloc race (2026-08-05 full audit, HIGH) — the previous
/// comment claiming this prevented data races was false for readers.
/// `pub(super)` so the sibling `env` module takes the same lock.
pub(super) static SETENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Sets an environment variable. Returns 1 on success, 0 on failure.
/// Thread-safe: uses a global mutex to serialize env var modifications,
/// preventing data races when called from multiple actor threads.
///
/// # Safety
/// Pointer arguments must be valid for the C ABI contract:
/// C strings must be NUL-terminated (unless documented otherwise),
/// result/free pointers must come from matching mimi runtime calls,
/// and lists/stat handles must be live values created by this runtime.
#[no_mangle]
pub unsafe extern "C" fn mimi_set_env(
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
    // Serialize env var writes under the global lock. Readers must take the
    // same lock (see SETENV_LOCK and env.rs mimi_getenv): POSIX setenv may
    // realloc the environ array, so any concurrent environment read races.
    let _lock = match SETENV_LOCK.lock() {
        Ok(guard) => guard,
        Err(_) => return 0,
    };
    // SAFETY: key_str and value_str are valid UTF-8 &strs (checked above).
    // std::env::set_var may reallocate the process-wide `environ` array;
    // that is exactly why SETENV_LOCK is held, and why every runtime
    // environment reader (mimi_getenv) takes the same lock. No other runtime
    // path reads the environment without the lock.
    unsafe { std::env::set_var(key_str, value_str) };
    1
}

#[cfg(test)]
mod tests {
    //! Regression tests for the 2026-08-05 audit fix (HIGH): mimi_listdir /
    //! mimi_walk_dir must hand mimi_list_free a libc::malloc'd element array
    //! (H1 pattern), not a Rust Vec buffer (allocator mismatch + list_cap
    //! data[-8] OOB).

    use super::*;

    /// Create `dir` with two files and one nested file; returns the dir path.
    fn make_tree(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("mimi_audit_fs_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), b"a").unwrap();
        std::fs::write(dir.join("b.txt"), b"b").unwrap();
        std::fs::write(dir.join("sub").join("c.txt"), b"c").unwrap();
        dir
    }

    fn list_to_strings(list: *mut MimiList) -> Vec<String> {
        // SAFETY: caller passes a list just returned by mimi_listdir /
        // mimi_walk_dir; `data` holds `len` NUL-terminated C strings.
        unsafe {
            let lst = &*list;
            let mut out = Vec::new();
            for i in 0..lst.len as isize {
                let p = *lst.data.offset(i);
                if !p.is_null() {
                    out.push(
                        CStr::from_ptr(p as *const std::ffi::c_char)
                            .to_string_lossy()
                            .into_owned(),
                    );
                }
            }
            out
        }
    }

    #[test]
    fn listdir_survives_mimi_list_free() {
        let dir = make_tree("listdir");
        let c_path = std::ffi::CString::new(dir.to_str().unwrap()).unwrap();
        let list = unsafe { mimi_listdir(c_path.as_ptr()) };
        assert!(!list.is_null());
        // H-26 flag contract: header-less owning list.
        // SAFETY: `list` was just returned by mimi_listdir.
        unsafe {
            assert!(!(*list).has_header);
        }
        let mut names = list_to_strings(list);
        names.sort();
        assert_eq!(names, vec!["a.txt", "b.txt", "sub"]);
        // The old Vec-buffer ABI failed exactly here: list_cap read data[-8]
        // OOB and mimi_list_free freed a Vec buffer via libc::free.
        unsafe {
            crate::runtime::mimi_list_free(list, true);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn walk_dir_survives_mimi_list_free() {
        let dir = make_tree("walk");
        let c_path = std::ffi::CString::new(dir.to_str().unwrap()).unwrap();
        let list = unsafe { mimi_walk_dir(c_path.as_ptr()) };
        assert!(!list.is_null());
        let mut paths = list_to_strings(list);
        paths.sort();
        assert_eq!(paths.len(), 3, "walk_dir must see a.txt, b.txt, sub/c.txt");
        assert!(paths.iter().any(|p| p.ends_with("a.txt")));
        assert!(paths.iter().any(|p| p.ends_with("b.txt")));
        let c_suffix = std::path::MAIN_SEPARATOR.to_string() + "c.txt";
        assert!(paths.iter().any(|p| p.ends_with(&c_suffix)));
        unsafe {
            crate::runtime::mimi_list_free(list, true);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn walk_dir_does_not_follow_symlink_cycle() {
        let dir = make_tree("walk_cycle");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&dir, dir.join("sub").join("back")).unwrap();
        let c_path = std::ffi::CString::new(dir.to_str().unwrap()).unwrap();
        let list = unsafe { mimi_walk_dir(c_path.as_ptr()) };
        assert!(!list.is_null());
        let paths = list_to_strings(list);
        // The cycle must not make the walker loop; it should still terminate
        // with the original three real files.
        assert_eq!(
            paths.len(),
            3,
            "walk must not follow symlink cycle: {paths:?}"
        );
        unsafe {
            crate::runtime::mimi_list_free(list, true);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn listdir_missing_dir_is_empty_not_null() {
        let c_path = std::ffi::CString::new("/nonexistent/mimi/audit/path").unwrap();
        let list = unsafe { mimi_listdir(c_path.as_ptr()) };
        assert!(!list.is_null());
        // SAFETY: list just allocated by mimi_listdir.
        unsafe {
            assert_eq!((*list).len, 0);
        }
        unsafe {
            crate::runtime::mimi_list_free(list, true);
        }
    }

    #[test]
    fn exec_safe_rejects_malformed_args_list() {
        let prog = std::ffi::CString::new("/bin/true").unwrap();
        // Negative len: must fail without touching data.
        let mut bad_len = MimiList {
            len: -1,
            data: std::ptr::null_mut(),
            owns_data: false,
            element_kind: ListElementKind::String,
            has_header: false,
        };
        let res = unsafe { mimi_exec_safe(prog.as_ptr(), &mut bad_len) };
        assert!(!res.is_null());
        // SAFETY: res is a valid MimiExecResult from mimi_exec_safe.
        unsafe {
            let stderr = CStr::from_ptr((*res).stderr).to_string_lossy().into_owned();
            assert!(
                stderr.contains("invalid args list"),
                "negative len must be rejected, stderr={stderr}"
            );
            mimi_exec_free(res);
        }

        // Non-null data is required when len > 0.
        let mut null_data = MimiList {
            len: 1,
            data: std::ptr::null_mut(),
            owns_data: false,
            element_kind: ListElementKind::String,
            has_header: false,
        };
        let res = unsafe { mimi_exec_safe(prog.as_ptr(), &mut null_data) };
        assert!(!res.is_null());
        // SAFETY: res is a valid MimiExecResult from mimi_exec_safe.
        unsafe {
            let stderr = CStr::from_ptr((*res).stderr).to_string_lossy().into_owned();
            assert!(
                stderr.contains("invalid args list"),
                "null data with len>0 must be rejected, stderr={stderr}"
            );
            mimi_exec_free(res);
        }

        // An empty list is always safe to run with no args, regardless of
        // element_kind (codegen lists only provide the {len, data} prefix).
        let mut empty = MimiList {
            len: 0,
            data: std::ptr::null_mut(),
            owns_data: false,
            element_kind: ListElementKind::I64,
            has_header: false,
        };
        let res = unsafe { mimi_exec_safe(prog.as_ptr(), &mut empty) };
        assert!(!res.is_null());
        // SAFETY: res is a valid MimiExecResult from mimi_exec_safe.
        unsafe {
            mimi_exec_free(res);
        }
    }
}
