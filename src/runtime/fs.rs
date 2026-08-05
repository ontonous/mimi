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
#[no_mangle]
pub extern "C" fn mimi_listdir(path: *const std::ffi::c_char) -> *mut MimiList {
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
    walk_dir_recursive(path_str, &mut results);
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
/// Uses shell interpretation (pipelines, variables, redirections).
///
/// Security note (HIGH): `cmd` is passed directly to `sh -c`. If `cmd`
/// contains user-controlled input, shell injection is possible. Only
/// use `mimi_exec` with trusted, hard-coded command strings. For
/// untrusted input, use `mimi_exec_safe` which avoids the shell.
/// Caller must free with `mimi_exec_free`.
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
/// stdout/stderr are allocated by alloc_c_string (mimi_alloc), so they are
/// freed through mimi_free — the matching deallocator under both normal and
/// miri builds (audit 2026-08-05, N-1).
#[no_mangle]
pub extern "C" fn mimi_exec_free(res: *mut MimiExecResult) {
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
        let list = mimi_listdir(c_path.as_ptr());
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
        crate::runtime::mimi_list_free(list, true);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn walk_dir_survives_mimi_list_free() {
        let dir = make_tree("walk");
        let c_path = std::ffi::CString::new(dir.to_str().unwrap()).unwrap();
        let list = mimi_walk_dir(c_path.as_ptr());
        assert!(!list.is_null());
        let mut paths = list_to_strings(list);
        paths.sort();
        assert_eq!(paths.len(), 3, "walk_dir must see a.txt, b.txt, sub/c.txt");
        assert!(paths.iter().any(|p| p.ends_with("a.txt")));
        assert!(paths.iter().any(|p| p.ends_with("b.txt")));
        let c_suffix = std::path::MAIN_SEPARATOR.to_string() + "c.txt";
        assert!(paths.iter().any(|p| p.ends_with(&c_suffix)));
        crate::runtime::mimi_list_free(list, true);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn listdir_missing_dir_is_empty_not_null() {
        let c_path = std::ffi::CString::new("/nonexistent/mimi/audit/path").unwrap();
        let list = mimi_listdir(c_path.as_ptr());
        assert!(!list.is_null());
        // SAFETY: list just allocated by mimi_listdir.
        unsafe {
            assert_eq!((*list).len, 0);
        }
        crate::runtime::mimi_list_free(list, true);
    }
}
