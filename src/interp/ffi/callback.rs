use super::super::ffi_runtime::FfiClosureRunner;
use super::super::*;
use crate::ast::*;
use crate::ffi::callback_table_remove;
use libffi::low::{self as ffi_low};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

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
// SAFETY: The runner pointer (tree-walker Interpreter or Bytecode VM) is
// only valid during the synchronous FFI call on the same thread. The closure
// value is cloned from the runner's environment and lives for the duration
// of the call.
thread_local! {
    pub(in crate::interp) static FFI_CALLBACK_CTX: RefCell<FfiCallbackCtx> = RefCell::new(FfiCallbackCtx {
        interp: None,
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
    /// Execution engine that can run a Mimi closure: either the tree-walker
    /// interpreter or the bytecode VM (0.33 FFI forwarding).
    pub(in crate::interp) interp: Option<*mut dyn super::super::ffi_runtime::FfiClosureRunner>,
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
pub(in crate::interp) struct CallbackTrampolineKeepalive {
    pub(in crate::interp) _closure: Box<libffi::middle::Closure<'static>>,
    pub(in crate::interp) _userdata: Box<i64>,
}
// SAFETY: trampoline memory is process-global and only dropped under the
// store mutex after active callbacks drain; no concurrent free of Closure.
unsafe impl Send for CallbackTrampolineKeepalive {}

/// Global entry for a registered callback (Mimi closure + trampoline keepalive).
pub(in crate::interp) struct GlobalCallbackEntry {
    pub(in crate::interp) closure: Value,
    pub(in crate::interp) ret_is_float: bool,
    pub(in crate::interp) arg_free_mask: Vec<bool>,
    pub(in crate::interp) arg_kinds: Vec<CallbackArgKind>,
    pub(in crate::interp) active_count: Arc<AtomicUsize>,
    /// R-C3: keeps the executable trampoline alive after the sync FFI call.
    pub(in crate::interp) keepalive: Option<CallbackTrampolineKeepalive>,
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
/// §12-#65 (closed 0.36.100): the old "callback slot TLS vs cross-thread"
/// contradiction is resolved by this store plus `CALLBACK_FILE` cross-thread
/// interpreter fallback and `mimi_callback_deregister` lifecycle.
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

pub(in crate::interp) fn global_callback_store() -> &'static Mutex<HashMap<i64, GlobalCallbackEntry>>
{
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
pub(in crate::interp) fn ensure_callback_file(file: &File) {
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
/// BytecodeProgram for cross-thread BytecodeClosure evaluation
/// (0.33 Phase D FFI forwarding).
///
/// 0.35.27 (C3) ownership model: `BytecodeClosure` carries its own program
/// `Arc` (see `Value::BytecodeClosure::program`), so a C library that stores
/// the callback function pointer and invokes it after the synchronous extern
/// call returned — even from another thread — evaluates against the closure's
/// own program, which stays alive as long as the closure does. There is no
/// global "latest program" pointer, hence no dangling pointer and no
/// VM-lifetime coupling. (The pre-C3 design stored a raw `*const
/// BytecodeProgram` globally — UAF when the owning VM dropped before a
/// delayed callback fired.)

fn encode_callback_result(result: Value, ret_is_float: bool) -> Result<i64, String> {
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

fn evaluate_cross_thread_callback(
    closure: &Value,
    args: Vec<Value>,
    ret_is_float: bool,
) -> Result<i64, String> {
    if let Value::BytecodeClosure { program, .. } = closure {
        // 0.35.27 (C3): the closure is self-contained — it carries its own
        // program Arc (proto indices are guaranteed to match this closure),
        // so the program outlives the VM that created it and can be evaluated
        // on any thread without a dangling pointer or a mismatched program.
        let mut vm = crate::interp::bytecode::vm::BytecodeVM::new(std::sync::Arc::clone(program));
        let result = vm
            .apply_closure_ffi(closure, args)
            .map_err(|e| format!("cross-thread callback evaluation error: {}", e))?;
        return encode_callback_result(result, ret_is_float);
    }
    // 0.33 Phase F: tree-walker Closure values are no longer produced.
    // All closures are BytecodeClosure; if we reach here it's a logic error.
    Err(format!(
        "cross-thread callback: unsupported closure type {} (tree-walker removed in 0.33)",
        closure
    ))
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
pub(in crate::interp) unsafe extern "C" fn mimi_callback_trampoline_fn(
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
            eprintln!("[mimi] FFI STRICT (IP-C5): refusing nested FFI callback reentrancy");
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
                    Value::String(Arc::new(String::new()))
                } else {
                    // SAFETY: free_mask decides ownership; for borrow, C keeps it.
                    let s = unsafe { std::ffi::CStr::from_ptr(cptr) }
                        .to_string_lossy()
                        .into_owned();
                    Value::String(Arc::new(s))
                }
            }
            CallbackArgKind::Int => {
                let n = *(arg_ptr as *const i64);
                Value::Int(n)
            }
        };
        mimi_args.push(val);
    }

    // Call the Mimi closure via the runner (tree-walker Interpreter or Bytecode VM)
    // P1-7 fix: Save runner pointer and clear it to prevent reentrancy UB.
    // If a nested callback (same thread) tries to re-enter during apply_closure_ffi,
    // it will see a null pointer and return gracefully instead of causing
    // a mutable borrow conflict on the same engine.
    let runner_ptr: Option<*mut dyn super::super::ffi_runtime::FfiClosureRunner> = FFI_CALLBACK_CTX
        .with(|c| {
            let mut ctx = c.borrow_mut();
            // take() reads the pointer AND clears it — prevents reentrancy UB.
            ctx.interp.take()
        });
    if runner_ptr.is_none() {
        // Cross-thread / async callback: the TLS runner context has been
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
        // interp F2: callback `string`/`CBuffer` args are treated as borrowed
        // (see `compute_arg_free_mask`); the mask is always false, so this loop
        // is retained for symmetry but performs no free. The decode already
        // copied the bytes into an `Arc<String>`, so freeing the C-side pointer
        // (often a static literal) would be heap corruption.
        for (i, &should_free) in arg_free_mask.iter().enumerate() {
            if should_free && i < nargs {
                let arg_slot = *args.add(i);
                if !arg_slot.is_null() {
                    // SAFETY: libffi passes a pointer to the argument slot. The slot
                    // contains the transferred C string pointer allocated by malloc/strdup.
                    let owned_ptr = unsafe { *(arg_slot as *const *mut libc::c_void) };
                    if !owned_ptr.is_null() {
                        unsafe { libc::free(owned_ptr) }; // SAFETY: owned_ptr 为 C 侧 malloc/strdup 分配（上方非空检查），libc::free 配对释放。
                    }
                }
            }
        }
        return;
    }
    // SAFETY: runner_ptr was just read from FFI_CALLBACK_CTX, which stores a
    // pointer to the runner driving the synchronous FFI call. The pointer
    // remains valid because that runner is still alive on the original stack
    // frame for the duration of this callback.
    // SAFETY: runner_ptr is the current thread's FFI_CALLBACK_CTX pointer, valid for this synchronous callback.
    // None was checked above (early return), so the unwrap via match cannot fire.
    let runner_ptr = match runner_ptr {
        Some(p) => p,
        None => return,
    };
    let runner = unsafe { &mut *runner_ptr }; // SAFETY: runner_ptr 为 FFI_CALLBACK_CTX 当前线程指针（None 已提前返回），同步回调期间原栈帧存活。
    let closure_result = runner.apply_closure_ffi(&closure, mimi_args);
    // Restore the runner pointer after the callback completes
    FFI_CALLBACK_CTX.with(|c| {
        c.borrow_mut().interp = Some(runner_ptr);
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

    // interp F2: callback `string`/`CBuffer` args are borrowed (see
    // `compute_arg_free_mask`); the mask is always false, so this loop performs
    // no free. The decode already copied the bytes into an `Arc<String>`.
    for (i, &should_free) in arg_free_mask.iter().enumerate() {
        if should_free && i < nargs {
            let arg_slot = *args.add(i);
            if !arg_slot.is_null() {
                // SAFETY: libffi passes a pointer to the argument slot. The slot
                // contains the transferred C string pointer allocated by malloc/strdup.
                let owned_ptr = unsafe { *(arg_slot as *const *mut libc::c_void) };
                if !owned_ptr.is_null() {
                    unsafe { libc::free(owned_ptr) }; // SAFETY: owned_ptr 为 C 侧 malloc/strdup 分配（上方非空检查），libc::free 配对释放。
                }
            }
        }
    }
}
