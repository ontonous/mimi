use super::super::*;
use super::helpers::{compute_arg_free_mask, FfiGuard};
use crate::ast::*;
use crate::ffi::{CALLBACK_TABLE, Errno};
use libffi::low::{self as ffi_low};
use libffi::middle::{Cif, Type as FfiType};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Mutex;

// F8: Thread-local context for synchronous callback invocation.
// Set before each FFI call that involves callbacks, cleared after.
// Maps callback_id -> (Mimi closure, ret_is_float, arg_free_mask).
// arg_free_mask[i] = true means callback arg i is a C-allocated string
// that Mimi takes ownership of and must free after the callback returns.
// SAFETY: The interpreter pointer is only valid during the synchronous
// FFI call on the same thread. The closure value is cloned from the
// interpreter's environment and lives for the duration of the call.
thread_local! {
    pub(in crate::interp) static FFI_CALLBACK_CTX: RefCell<FfiCallbackCtx> = RefCell::new(FfiCallbackCtx {
        interp: std::ptr::null(),
        entries: HashMap::new(),
    });
}

pub(in crate::interp) struct FfiCallbackCtx {
    pub(in crate::interp) interp: *const Interpreter<'static>,
    // (closure, ret_is_float, arg_free_mask: Vec<bool>)
    pub(in crate::interp) entries: HashMap<i64, (Value, bool, Vec<bool>)>,
}

/// F3: Global fallback store for asynchronous/off-thread callbacks.
/// When C stores a callback function pointer and invokes it after the
/// synchronous FFI call returns, the thread-local context has been cleared.
/// This global store keeps closures alive so the trampoline can still find
/// them. Entries persist until explicitly deregistered via
/// `mimi_callback_deregister` or until process exit.
static CALLBACK_GLOBAL_STORE: std::sync::LazyLock<Mutex<HashMap<i64, (Value, bool, Vec<bool>)>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// F3: C-ABI function to deregister an async callback and free its resources.
/// Should be called by C code when the stored function pointer is no longer
/// needed (e.g., when unregistering an event handler).
/// Safe to call from any thread.
#[no_mangle]
pub extern "C" fn mimi_callback_deregister(callback_id: i64) {
    CALLBACK_GLOBAL_STORE.lock().unwrap_or_else(|e| e.into_inner()).remove(&callback_id);
    CALLBACK_TABLE.remove(callback_id);
    FFI_CALLBACK_CTX.with(|c| {
        c.borrow_mut().entries.remove(&callback_id);
    });
}

// F8: C callback trampoline invoked by a libffi closure.
// Reads the Mimi closure from the thread-local context by callback_id,
// converts C args to Mimi Values, calls the closure, and writes the result.
// SAFETY: Called from C (extern "C" context) during a synchronous FFI call.
unsafe extern "C" fn mimi_callback_trampoline_fn(
    cif: &ffi_low::ffi_cif,
    result: &mut i64,
    args: *const *const std::ffi::c_void,
    userdata: &i64,
) {
    let callback_id = *userdata;
    // F3: Fast path — check thread-local context first (synchronous callbacks).
    // If not found, fall back to the global store (async/off-thread callbacks).
    let entry = FFI_CALLBACK_CTX.with(|c| {
        let ctx = c.borrow();
        ctx.entries.get(&callback_id).cloned()
    });
    let (closure, ret_is_float, arg_free_mask) = match entry {
        Some(e) => e,
        None => {
            let global = CALLBACK_GLOBAL_STORE.lock().unwrap_or_else(|e| e.into_inner());
            match global.get(&callback_id).cloned() {
                Some(e) => e,
                None => {
                    *result = 0;
                    return;
                }
            }
        }
    };

    // Extract C arguments from raw void pointers.
    let nargs = cif.nargs as usize;
    let mut mimi_args: Vec<Value> = Vec::with_capacity(nargs);
    for i in 0..nargs {
        let arg_ptr = *args.add(i);
        if arg_ptr.is_null() {
            mimi_args.push(Value::Int(0));
            continue;
        }
        // For V1, treat all args as i64. Float is handled via to_bits.
        let val = *(arg_ptr as *const i64);
        mimi_args.push(Value::Int(val));
    }

    // Call the Mimi closure via interpreter
    let interp_ptr = FFI_CALLBACK_CTX.with(|c| c.borrow().interp);
    if interp_ptr.is_null() {
        // F3: Closure found in global store but no interpreter available.
        // This happens when C invokes the callback off-thread or after the
        // original synchronous FFI call has returned and TLS was cleared.
        // Full async/off-thread callback evaluation is a known limitation
        // tracked in the MimiSpec FFI roadmap.
        eprintln!(
            "[mimi] WARNING: callback {} invoked without an interpreter context. \
             Returning 0. Async/off-thread callback evaluation is not yet supported.",
            callback_id,
        );
        *result = 0;
        return;
    }
    let interp = &mut *(interp_ptr as *mut Interpreter<'static>);
    let closure_result = interp.apply_closure_ffi(&closure, mimi_args);
    match closure_result {
        Ok(val) => {
            if ret_is_float {
                if let Value::Float(f) = val {
                    *result = f.to_bits() as i64;
                } else if let Value::Int(n) = val {
                    *result = (n as f64).to_bits() as i64;
                }
            } else {
                *result = match val {
                    Value::Int(n) => n,
                    Value::Bool(b) => b as i64,
                    Value::Float(f) => f.to_bits() as i64,
                    Value::Unit => 0,
                    _ => {
                        *result = i64::MIN;
                        return;
                    }
                };
            }
        }
        Err(_) => {
            *result = i64::MIN;
        }
    }

    // F6: Free C-allocated string pointers that Mimi takes ownership of.
    // Convention: callback args typed as `string` / `RawString` / `CBuffer`
    // are treated as transfer ownership — C allocated them, Mimi frees them.
    for (i, &should_free) in arg_free_mask.iter().enumerate() {
        if should_free && i < nargs {
            let arg_ptr = *args.add(i);
            if !arg_ptr.is_null() {
                unsafe { libc::free(arg_ptr as *mut libc::c_void); }
            }
        }
    }
}

impl<'a> Interpreter<'a> {
    /// F8: Apply a Mimi closure value to arguments from within a C callback context.
    /// Mirrors `apply_closure` in call.rs but designed for &self usage from a
    /// C trampoline via raw pointer.
    pub(crate) fn apply_closure_ffi(&mut self, closure: &Value, args: Vec<Value>) -> Result<Value, Errno> {
        match closure {
            Value::Closure { params, body, captured, .. } =>
                self.apply_closure_inner(params, body, captured, args).map_err(|e| Errno::from(e.to_string())),
            _ => Err(Errno::Generic(format!("expected a closure, found {}", closure))),
        }
    }

    /// F8: Convert a Mimi closure value to a C-compatible callback function pointer.
    /// Registers the closure with the global callback table and creates a
    /// dynamically generated trampoline via libffi.
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
                let cif = Cif::new(cif_arg_types.into_iter(), cif_ret);

                // Register with CALLBACK_TABLE so the trampoline can find it
                // Use a dummy invoker (the real invocation is via thread-local ctx)
                let cb_id = CALLBACK_TABLE.register(
                    Some(Box::new(|_id: i64, _args: &[i64]| -> i64 { 0 })),
                );
                callback_ids.push(cb_id);

                // F3: Store the closure in BOTH the thread-local context (fast path
                // for synchronous callbacks) and the global store (fallback for async/
                // off-thread callbacks where TLS has been cleared).
                let arg_free_mask = compute_arg_free_mask(param_types);
                FFI_CALLBACK_CTX.with(|c| {
                    let mut ctx = c.borrow_mut();
                    ctx.entries.insert(cb_id, (closure.clone(), ret_is_float, arg_free_mask.clone()));
                });
                CALLBACK_GLOBAL_STORE.lock().unwrap_or_else(|e| e.into_inner())
                    .insert(cb_id, (closure, ret_is_float, arg_free_mask));

                // Create a libffi Closure that generates a C-compatible function pointer.
                // The userdata (callback_id) must outlive the closure.
                // Box::leak gives us a 'static reference that is valid as long as
                // the Box is not re-created (we reclaim it via Box::from_raw below).
                let userdata = Box::new(cb_id);
                // SAFETY: Box::leak intentionally leaks the Box allocation. The
                // memory remains valid until reclaimed by Box::from_raw in the
                // FfiGuard constructor below. The libffi Closure captures this
                // reference and is boxed alongside it in FfiGuard::CallbackClosure;
                // when the FfiGuard is dropped, Box::from_raw re-owns the allocation
                // and Box::new(ffi_closure) drops the closure, so the reference
                // never dangles.
                let userdata_ptr = Box::into_raw(userdata);
                let cb_ref_static: &'static i64 = unsafe { &*userdata_ptr };

                let ffi_closure = libffi::middle::Closure::new(
                    cif,
                    mimi_callback_trampoline_fn as ffi_low::Callback<i64, i64>,
                    cb_ref_static,
                );

                let code_ptr_ref = ffi_closure.code_ptr();
                // code_ptr_ref is &unsafe extern "C" fn() — a reference to the generated
                // trampoline function pointer. We convert it to a raw i64 address.
                let fn_ptr_val: unsafe extern "C" fn() = *code_ptr_ref;
                let fn_ptr = fn_ptr_val as i64;

                // Keep the closure and its userdata alive for the duration of the C call
                ffi_guards.push(FfiGuard::CallbackClosure {
                    closure: Box::new(ffi_closure),
                    // SAFETY: userdata_ptr was obtained from Box::into_raw above.
                    // Box::from_raw reclaims ownership so the Box is dropped when
                    // FfiGuard drops (after the closure, ensuring the reference
                    // inside the closure is valid during Closure::drop).
                    userdata: unsafe { Box::from_raw(userdata_ptr) },
                });

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
