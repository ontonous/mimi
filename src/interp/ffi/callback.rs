use super::super::*;
use super::helpers::{compute_arg_free_mask, compute_arg_kinds, FfiGuard};
use crate::ast::*;
use crate::ffi::{callback_table_register, callback_table_remove, Errno};
use libffi::low::{self as ffi_low};
use libffi::middle::{Cif, Type as FfiType};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Wrapper around `*const File` for a process-static callback AST.
///
/// IP-H3: `File` is not inherently `Sync`, but this pointer is only used for
/// **immutable** AST reads after a one-time `Box::into_raw` leak. No thread
/// mutates the `File` after install; concurrent readers only traverse AST
/// nodes that are never written. Do not use this pattern for mutable tables.
///
/// SAFETY: The File is leaked and lives for the process lifetime.
/// Callers must not mutate through this pointer.
#[derive(Copy, Clone)]
struct SendFilePtr(*const File);
// SAFETY: IP-H3 — immutable post-leak AST; no concurrent mutation of File.
unsafe impl Send for SendFilePtr {}
// SAFETY: IP-H3 — same as Send; only immutable AST traversal after install.
unsafe impl Sync for SendFilePtr {}

// F8: Thread-local context for synchronous callback invocation.
// Set before each FFI call that involves callbacks, cleared after.
// Maps callback_id -> (Mimi closure, ret_is_float, arg_free_mask, arg_kinds).
// arg_free_mask[i] = true means callback arg i is a C-allocated string
// that Mimi takes ownership of and must free after the callback returns.
// arg_kinds[i] selects how to decode the raw C argument (IP-H4).
// SAFETY: The interpreter pointer is only valid during the synchronous
// FFI call on the same thread. The closure value is cloned from the
// interpreter's environment and lives for the duration of the call.
thread_local! {
    pub(in crate::interp) static FFI_CALLBACK_CTX: RefCell<FfiCallbackCtx> = RefCell::new(FfiCallbackCtx {
        interp: std::ptr::null_mut(),
        entries: HashMap::new(),
        reentrancy_depth: 0,
    });
}

/// How to decode a C callback argument from the raw void* slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::interp) enum CallbackArgKind {
    Int,
    Float,
    /// C string pointer (`*const c_char`), free if free_mask says so.
    CString,
}

pub(in crate::interp) struct FfiCallbackCtx {
    pub(in crate::interp) interp: *mut Interpreter<'static>,
    // (closure, ret_is_float, arg_free_mask, arg_kinds)
    pub(in crate::interp) entries: HashMap<i64, (Value, bool, Vec<bool>, Vec<CallbackArgKind>)>,
    /// Nested trampoline depth on this thread (IP-C5 soft mitigation).
    pub(in crate::interp) reentrancy_depth: u32,
}

use std::sync::Mutex;

/// R-C3: libffi machine-code trampoline + userdata that must outlive any
/// delayed C callback. Stored globally until `mimi_callback_deregister`.
///
/// SAFETY: libffi Closure is not Send, but we only access it under the
/// global store mutex and never move the trampoline across threads — only
/// the function pointer is shared with C. Marking Send is required for
/// the static Mutex map.
struct CallbackTrampolineKeepalive {
    _closure: Box<libffi::middle::Closure<'static>>,
    _userdata: Box<i64>,
}
// SAFETY: trampoline memory is process-global and only dropped under the
// store mutex after active callbacks drain; no concurrent free of Closure.
unsafe impl Send for CallbackTrampolineKeepalive {}

/// Global entry for a registered callback (Mimi closure + trampoline keepalive).
struct GlobalCallbackEntry {
    closure: Value,
    ret_is_float: bool,
    arg_free_mask: Vec<bool>,
    arg_kinds: Vec<CallbackArgKind>,
    active_count: Arc<AtomicUsize>,
    /// R-C3: keeps the executable trampoline alive after the sync FFI call.
    keepalive: Option<CallbackTrampolineKeepalive>,
}

impl Clone for GlobalCallbackEntry {
    fn clone(&self) -> Self {
        // Keepalive is not cloneable (owns the trampoline); clone only the
        // callable payload used by the trampoline lookup path.
        Self {
            closure: self.closure.clone(),
            ret_is_float: self.ret_is_float,
            arg_free_mask: self.arg_free_mask.clone(),
            arg_kinds: self.arg_kinds.clone(),
            active_count: Arc::clone(&self.active_count),
            keepalive: None,
        }
    }
}

/// F3: Global fallback store for callbacks — accessible from any thread.
/// When C stores a callback function pointer and invokes it after the
/// synchronous FFI call returns, the thread-local context has been cleared.
/// This global store keeps closures alive so the trampoline can still find
/// them from any thread. Entries persist until explicitly deregistered via
/// `mimi_callback_deregister`.
///
/// FFI-10: Entry includes Arc<AtomicUsize> "active call" counter.
/// trampoline increments before invoking closure, decrements after.
/// deregister waits for count == 0 before removing the entry.
static CALLBACK_GLOBAL_STORE: std::sync::OnceLock<Mutex<HashMap<i64, GlobalCallbackEntry>>> =
    std::sync::OnceLock::new();

fn global_callback_store() -> &'static Mutex<HashMap<i64, GlobalCallbackEntry>> {
    CALLBACK_GLOBAL_STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// GLOBAL: Stored program File for cross-thread/async callback evaluation.
/// When the TLS interpreter context is null (callback invoked from a different
/// thread or after the synchronous FFI call completed), we create a temporary
/// Interpreter from this stored File and evaluate the closure.
/// The File is leaked once (Box::leak) at first callback registration time
/// and lives for the process lifetime.
static CALLBACK_FILE: std::sync::OnceLock<Mutex<Option<SendFilePtr>>> = std::sync::OnceLock::new();

fn callback_file() -> &'static Mutex<Option<SendFilePtr>> {
    CALLBACK_FILE.get_or_init(|| Mutex::new(None))
}

/// Leak a clone of the program File into the global callback store.
/// Called from value_to_ffi_callback to enable cross-thread evaluation.
fn ensure_callback_file(file: &File) {
    let mut store = callback_file().lock().unwrap_or_else(|e| e.into_inner());
    if store.is_some() {
        return;
    }
    let leaked = Box::into_raw(Box::new(file.clone()));
    *store = Some(SendFilePtr(leaked as *const File));
}

/// Evaluate a Mimi closure from a cross-thread callback context.
/// Creates a temporary Interpreter from the globally stored program file,
/// evaluates the closure, and returns the result as an i64.
fn evaluate_cross_thread_callback(
    closure: &Value,
    args: Vec<Value>,
    ret_is_float: bool,
) -> Result<i64, String> {
    let file_ptr = {
        let store = callback_file().lock().unwrap_or_else(|e| e.into_inner());
        store.as_ref().map(|s| s.0).ok_or_else(|| {
            "no program file registered for cross-thread callback evaluation".to_string()
        })?
    };
    // SAFETY: The File was Box::leaked and is valid for 'static.
    let file = unsafe { &*file_ptr };
    let mut interp = Interpreter::new(file);
    interp.verify_contracts = false;
    interp.verify_ffi = false;
    let result = interp
        .apply_closure_ffi(closure, args)
        .map_err(|e| format!("cross-thread callback evaluation error: {}", e))?;
    if ret_is_float {
        match result {
            Value::Float(f) => Ok(f.to_bits() as i64),
            Value::Int(n) => Ok((n as f64).to_bits() as i64),
            _ => Err(format!(
                "cross-thread callback: expected float return, got {}",
                result
            )),
        }
    } else {
        match result {
            Value::Int(n) => Ok(n),
            Value::Bool(b) => Ok(b as i64),
            Value::Float(f) => Ok(f.to_bits() as i64),
            Value::Unit => Ok(0),
            _ => Err(format!(
                "cross-thread callback: unsupported return type: {}",
                result
            )),
        }
    }
}

/// F3: C-ABI function to deregister an async callback and free its resources.
/// Should be called by C code when the stored function pointer is no longer
/// needed (e.g., when unregistering an event handler).
/// Safe to call from any thread.
/// FFI-10: Waits for any in-flight callback invocation to complete before
/// removing the entry, preventing the C function pointer from becoming a
/// dangling pointer while a callback is still running.
/// F-18: Lock ordering: always acquire CALLBACK_TABLE before CALLBACK_GLOBAL_STORE
/// to match the registration order (callback_table_register → global_callback_store).
/// This prevents deadlock when multiple threads register/deregister concurrently.
#[no_mangle]
pub extern "C" fn mimi_callback_deregister(callback_id: i64) {
    callback_table_remove(callback_id);
    // FFI-10: Extract the active-count Arc and remove the entry BEFORE waiting.
    // FFI-BUG-3 fix: Removing the entry first prevents new calls from finding
    // it and incrementing the count during the spin-drain loop (TOCTOU window).
    // Remove from store but keep the entry (and trampoline) until drain completes.
    let removed = {
        let mut store = global_callback_store()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        store.remove(&callback_id)
    };
    let Some(entry) = removed else {
        return;
    };
    // Spin until no in-flight calls remain (trampoline still valid via entry).
    loop {
        let n = entry.active_count.load(Ordering::Acquire);
        if n == 0 {
            break;
        }
        std::hint::spin_loop();
    }
    // Drop entry (and R-C3 keepalive) after drain.
    drop(entry);
    FFI_CALLBACK_CTX.with(|c| {
        c.borrow_mut().entries.remove(&callback_id);
    });
}

// F8: C callback trampoline invoked by a libffi closure.
// Reads the Mimi closure from the thread-local context by callback_id,
// converts C args to Mimi Values, calls the closure, and writes the result.
// SAFETY: Called from C (extern "C" context) during a synchronous FFI call.
// The entire body is wrapped in catch_unwind so no Rust panic can cross
// the C-ABI boundary (which would be undefined behavior).
unsafe extern "C" fn mimi_callback_trampoline_fn(
    cif: &ffi_low::ffi_cif,
    result: &mut i64,
    args: *const *const std::ffi::c_void,
    userdata: &i64,
) {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: args and userdata are valid for the duration of this call
        // because C holds the reference until the trampoline returns.
        unsafe { callback_trampoline_inner(cif, result, args, userdata) }
    }));
    if outcome.is_err() {
        eprintln!("[mimi] FFI safety: Rust panic caught in C callback trampoline");
        // IP-C4: i64::MIN is a legal integer return; use 0 as error sentinel.
        *result = 0;
    }
}

/// Inner body of the callback trampoline, extracted for catch_unwind wrapping.
/// SAFETY: args and interp_ptr are raw pointers that must be valid.
unsafe fn callback_trampoline_inner(
    cif: &ffi_low::ffi_cif,
    result: &mut i64,
    args: *const *const std::ffi::c_void,
    userdata: &i64,
) {
    // FFI-10: RAII guard — increments active count on creation, decrements on drop.
    struct ActiveCountGuard(Option<Arc<AtomicUsize>>);
    impl ActiveCountGuard {
        fn new(cnt: &Arc<AtomicUsize>) -> Self {
            cnt.fetch_add(1, Ordering::Acquire);
            ActiveCountGuard(Some(Arc::clone(cnt)))
        }
    }
    impl Drop for ActiveCountGuard {
        fn drop(&mut self) {
            if let Some(cnt) = &self.0 {
                cnt.fetch_sub(1, Ordering::Release);
            }
        }
    }

    let callback_id = *userdata;
    // IP-C5: track nested trampoline depth; warn once when re-entering while
    // the parent still holds the interpreter (interp is cleared during apply).
    let reentered = FFI_CALLBACK_CTX.with(|c| {
        let mut ctx = c.borrow_mut();
        let was = ctx.reentrancy_depth;
        ctx.reentrancy_depth = was.saturating_add(1);
        was > 0
    });
    if reentered {
        // IP-C5: under MIMI_FFI_STRICT refuse nested trampolines (return 0).
        let strict = std::env::var("MIMI_FFI_STRICT")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        if strict {
            eprintln!(
                "[mimi] FFI STRICT (IP-C5): refusing nested FFI callback reentrancy"
            );
            *result = 0;
            return;
        }
        static REENT_WARNED: std::sync::atomic::AtomicBool =
            std::sync::atomic::AtomicBool::new(false);
        if !REENT_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            eprintln!(
                "[mimi] WARNING: nested FFI callback reentrancy detected (IP-C5). \
                 Nested callbacks cannot share the live interpreter; side effects \
                 may be lost or evaluated on a temporary interpreter. \
                 Set MIMI_FFI_STRICT=1 to refuse."
            );
        }
    }
    struct DepthGuard;
    impl Drop for DepthGuard {
        fn drop(&mut self) {
            FFI_CALLBACK_CTX.with(|c| {
                let mut ctx = c.borrow_mut();
                ctx.reentrancy_depth = ctx.reentrancy_depth.saturating_sub(1);
            });
        }
    }
    let _depth_guard = DepthGuard;

    // F3: Fast path — check thread-local context first (synchronous callbacks).
    // If not found, fall back to the global store (async/off-thread callbacks).
    let entry = FFI_CALLBACK_CTX.with(|c| {
        let ctx = c.borrow();
        ctx.entries.get(&callback_id).cloned()
    });

    // Look up closure + active guard (bound for RAII Drop semantics)
    #[allow(unused_variables)]
    let (closure, ret_is_float, arg_free_mask, arg_kinds, active_guard) = match entry {
        Some((closure, ret_is_float, arg_free_mask, arg_kinds)) => {
            // TLS entry — use no-op active guard (global store count not affected)
            (
                closure,
                ret_is_float,
                arg_free_mask,
                arg_kinds,
                ActiveCountGuard(None),
            )
        }
        None => {
            // Global store entry — increment and track the count
            let entry = {
                let store = global_callback_store()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                match store.get(&callback_id).cloned() {
                    Some(e) => e,
                    None => {
                        *result = 0;
                        return;
                    }
                }
            };
            let cnt = Arc::clone(&entry.active_count);
            (
                entry.closure,
                entry.ret_is_float,
                entry.arg_free_mask,
                entry.arg_kinds,
                ActiveCountGuard::new(&cnt),
            )
        }
    };

    // active_guard is live here — if we return early it will be dropped (decremented).
    // Extract C arguments from raw void pointers using declared kinds (IP-H4).
    let nargs = cif.nargs as usize;
    let mut mimi_args: Vec<Value> = Vec::with_capacity(nargs);
    for i in 0..nargs {
        let arg_ptr = *args.add(i);
        if arg_ptr.is_null() {
            mimi_args.push(Value::Int(0));
            continue;
        }
        let kind = arg_kinds.get(i).copied().unwrap_or(CallbackArgKind::Int);
        let val = match kind {
            CallbackArgKind::Float => {
                // ABI: f64 passed by value in the slot (or as bits via libffi).
                let bits = *(arg_ptr as *const i64);
                Value::Float(f64::from_bits(bits as u64))
            }
            CallbackArgKind::CString => {
                let cptr = *(arg_ptr as *const *const std::ffi::c_char);
                if cptr.is_null() {
                    Value::String(String::new())
                } else {
                    // SAFETY: free_mask decides ownership; for borrow, C keeps it.
                    let s = unsafe { std::ffi::CStr::from_ptr(cptr) }
                        .to_string_lossy()
                        .into_owned();
                    Value::String(s)
                }
            }
            CallbackArgKind::Int => {
                let n = *(arg_ptr as *const i64);
                Value::Int(n)
            }
        };
        mimi_args.push(val);
    }

    // Call the Mimi closure via interpreter
    // P1-7 fix: Save interp pointer and clear it to prevent reentrancy UB.
    // If a nested callback (same thread) tries to re-enter during apply_closure_ffi,
    // it will see a null interp and return gracefully instead of causing
    // a mutable borrow conflict on the same Interpreter.
    let interp_ptr = {
        let mut interp_ptr_copy: *mut Interpreter<'static> = std::ptr::null_mut();
        FFI_CALLBACK_CTX.with(|c| {
            let mut ctx = c.borrow_mut();
            interp_ptr_copy = ctx.interp;
            ctx.interp = std::ptr::null_mut(); // clear — prevents reentrancy
        });
        interp_ptr_copy
    };
    if interp_ptr.is_null() {
        // Cross-thread / async callback: the TLS interpreter context has been
        // cleared. Try to evaluate using a temporary Interpreter from the
        // globally stored program file. If that also fails, return 0 (IP-C4).
        let xt_result = evaluate_cross_thread_callback(&closure, mimi_args, ret_is_float);
        match xt_result {
            Ok(val) => {
                *result = val;
            }
            Err(msg) => {
                eprintln!(
                    "[mimi] WARNING: cross-thread callback {} evaluation failed: {}. \
                     Returning 0 (IP-C4: i64::MIN is a legal C return).",
                    callback_id, msg,
                );
                *result = 0;
            }
        }
        // SAFETY: arg_free_mask marks args that were transferred from C as
        // owned strings (C malloc / strdup). Free with libc::free only (IP-C3).
        for (i, &should_free) in arg_free_mask.iter().enumerate() {
            if should_free && i < nargs {
                let arg_slot = *args.add(i);
                if !arg_slot.is_null() {
                    // SAFETY: libffi passes a pointer to the argument slot. The slot
                    // contains the transferred C string pointer allocated by malloc/strdup.
                    let owned_ptr = unsafe { *(arg_slot as *const *mut libc::c_void) };
                    if !owned_ptr.is_null() {
                        unsafe { libc::free(owned_ptr) };
                    }
                }
            }
        }
        return;
    }
    // SAFETY: interp_ptr was just read from FFI_CALLBACK_CTX, which stores a
    // pointer to the Interpreter driving the synchronous FFI call. The pointer
    // remains valid because that Interpreter is still alive on the original stack
    // frame for the duration of this callback.
    // SAFETY: interp_ptr is the current thread's FFI_CALLBACK_CTX pointer, valid for this synchronous callback.
    let interp = unsafe { &mut *interp_ptr };
    let closure_result = interp.apply_closure_ffi(&closure, mimi_args);
    // Restore the interp pointer after the callback completes
    FFI_CALLBACK_CTX.with(|c| {
        c.borrow_mut().interp = interp_ptr;
    });
    match closure_result {
        Ok(val) => {
            // FFI-DESIGN-3 / IP-C4: on type mismatch use NaN bits for float slots
            // and 0 for integer slots — never i64::MIN (legal C return).
            if ret_is_float {
                match val {
                    Value::Float(f) => *result = f.to_bits() as i64,
                    Value::Int(n) => *result = (n as f64).to_bits() as i64,
                    _ => {
                        *result = f64::NAN.to_bits() as i64;
                        return;
                    }
                }
            } else {
                *result = match val {
                    Value::Int(n) => n,
                    Value::Bool(b) => b as i64,
                    Value::Float(f) => f.to_bits() as i64,
                    Value::Unit => 0,
                    _ => 0,
                };
            }
        }
        Err(_) => {
            *result = if ret_is_float {
                f64::NAN.to_bits() as i64
            } else {
                0
            };
        }
    }
    // active_guard dropped here — decrements count

    // F6: Free C-allocated string pointers that Mimi takes ownership of.
    // SAFETY: arg_free_mask marks C-owned strings (malloc/strdup). Free with
    // libc::free only — never CString::from_raw (IP-C3 allocator match).
    for (i, &should_free) in arg_free_mask.iter().enumerate() {
        if should_free && i < nargs {
            let arg_slot = *args.add(i);
            if !arg_slot.is_null() {
                // SAFETY: libffi passes a pointer to the argument slot. The slot
                // contains the transferred C string pointer allocated by malloc/strdup.
                let owned_ptr = unsafe { *(arg_slot as *const *mut libc::c_void) };
                if !owned_ptr.is_null() {
                    unsafe { libc::free(owned_ptr) };
                }
            }
        }
    }
}

impl<'a> Interpreter<'a> {
    /// F8: Apply a Mimi closure value to arguments from within a C callback context.
    /// Mirrors `apply_closure` in call.rs but designed for &self usage from a
    /// C trampoline via raw pointer.
    pub(crate) fn apply_closure_ffi(
        &mut self,
        closure: &Value,
        args: Vec<Value>,
    ) -> Result<Value, Errno> {
        match closure {
            Value::Closure {
                params,
                body,
                captured,
                ..
            } => self
                .apply_closure_inner(params, body, captured, args)
                .map_err(|e| Errno::from(e.to_string())),
            _ => Err(Errno::Generic(format!(
                "expected a closure, found {}",
                closure
            ))),
        }
    }

    /// F8: Convert a Mimi closure value to a C-compatible callback function pointer.
    /// Registers the closure with the global callback table and creates a
    /// dynamically generated trampoline via libffi.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::interp) fn value_to_ffi_callback(
        &self,
        arg: &Value,
        param_types: &[Type],
        ret_type: &Type,
        _string_guards: &mut Vec<std::ffi::CString>,
        _shared_handles: &mut Vec<std::sync::Arc<crate::ffi::runtime::SharedHandle>>,
        ffi_guards: &mut Vec<FfiGuard>,
        callback_ids: &mut Vec<i64>,
    ) -> Result<i64, Errno> {
        // Ensure the program File is stored for cross-thread callback evaluation.
        ensure_callback_file(self.file);
        match arg {
            Value::Closure { .. } => {
                let closure = arg.clone();
                let ret_is_float = matches!(ret_type, Type::Name(name, _) if name == "f64");

                // Build CIF matching the callback signature
                let mut cif_arg_types: Vec<FfiType> = Vec::with_capacity(param_types.len());
                for pt in param_types {
                    match pt {
                        Type::Name(name, _) if name == "f64" => {
                            cif_arg_types.push(FfiType::f64());
                        }
                        _ => {
                            cif_arg_types.push(FfiType::i64());
                        }
                    }
                }
                let cif_ret = if ret_is_float {
                    FfiType::f64()
                } else {
                    FfiType::i64()
                };
                let cif = Cif::new(cif_arg_types, cif_ret);

                // Register with CALLBACK_TABLE so the trampoline can find it
                // Use a dummy invoker (the real invocation is via thread-local ctx)
                let cb_id = callback_table_register(
                    Some(Box::new(|_id: i64, _args: &[i64]| -> i64 { 0 })),
                );
                callback_ids.push(cb_id);

                // F3: Store the closure in BOTH the thread-local context (fast path
                // for synchronous callbacks) and the global store (fallback for async/
                // off-thread callbacks where TLS has been cleared).
                let arg_free_mask = compute_arg_free_mask(param_types);
                let arg_kinds = compute_arg_kinds(param_types);
                // FFI-10: Per-callback active-call counter for deregister race prevention.
                let active_count = Arc::new(AtomicUsize::new(0));
                FFI_CALLBACK_CTX.with(|c| {
                    let mut ctx = c.borrow_mut();
                    ctx.entries.insert(
                        cb_id,
                        (
                            closure.clone(),
                            ret_is_float,
                            arg_free_mask.clone(),
                            arg_kinds.clone(),
                        ),
                    );
                });

                // Create a libffi Closure that generates a C-compatible function pointer.
                // R-C3: userdata + Closure must outlive any delayed C callback —
                // store them in CALLBACK_GLOBAL_STORE, not only FfiGuard.
                let userdata = Box::new(cb_id);
                let userdata_ptr = Box::into_raw(userdata);
                // SAFETY: userdata_ptr from Box::into_raw; reclaimed into keepalive below.
                let cb_ref_static: &'static i64 = unsafe { &*userdata_ptr };

                let ffi_closure = libffi::middle::Closure::new(
                    cif,
                    mimi_callback_trampoline_fn as ffi_low::Callback<i64, i64>,
                    cb_ref_static,
                );

                let code_ptr_ref = ffi_closure.code_ptr();
                // SAFETY: code_ptr_ref points to the libffi-generated trampoline.
                let fn_ptr_val: unsafe extern "C" fn() = *code_ptr_ref;
                let fn_ptr = fn_ptr_val as usize as i64;

                // SAFETY: reclaim userdata Box into keepalive alongside Closure.
                let keepalive = CallbackTrampolineKeepalive {
                    _closure: Box::new(ffi_closure),
                    _userdata: unsafe { Box::from_raw(userdata_ptr) },
                };

                if let Ok(mut store) = global_callback_store().lock() {
                    store.insert(
                        cb_id,
                        GlobalCallbackEntry {
                            closure,
                            ret_is_float,
                            arg_free_mask,
                            arg_kinds,
                            active_count: Arc::clone(&active_count),
                            keepalive: Some(keepalive),
                        },
                    );
                }

                // FfiGuard no longer owns the trampoline (global store does).
                // Keep a no-op guard slot unused — callers still pass ffi_guards.
                let _ = ffi_guards;

                Ok(fn_ptr)
            }
            Value::Int(n) => {
                // Already an opaque function pointer (passed through from a previous call)
                Ok(*n)
            }
            other => Err(Errno::Generic(format!(
                "FFI safety: expected a closure or function pointer for callback parameter, found {}",
                other
            ))),
        }
    }
}
