use super::*;
use crate::ffi::{FfiArgContract, FfiContract, FfiRetContract, CAP_TABLE, SHARED_TABLE, CALLBACK_TABLE, Errno};
use libffi::middle::{Cif, Type as FfiType, CodePtr, arg as ffi_arg};
use libffi::low::{self as ffi_low};
use std::cell::RefCell;
use std::collections::HashMap;

/// Global fork lock: acquired before fork() to prevent concurrent
/// FFI operations (thread pool, callbacks) during the fork window.
/// Held across fork(), released in parent and child via pthread_atfork.
static FORK_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
fn ensure_fork_lock() -> &'static std::sync::Mutex<()> {
    FORK_LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

// F8: Thread-local context for synchronous callback invocation.
// Set before each FFI call that involves callbacks, cleared after.
// Maps callback_id -> (Mimi closure, ret_is_float, arg_free_mask).
// arg_free_mask[i] = true means callback arg i is a C-allocated string
// that Mimi takes ownership of and must free after the callback returns.
// SAFETY: The interpreter pointer is only valid during the synchronous
// FFI call on the same thread. The closure value is cloned from the
// interpreter's environment and lives for the duration of the call.
thread_local! {
    static FFI_CALLBACK_CTX: RefCell<FfiCallbackCtx> = RefCell::new(FfiCallbackCtx {
        interp: std::ptr::null(),
        entries: HashMap::new(),
    });
}

struct FfiCallbackCtx {
    interp: *const Interpreter<'static>,
    // (closure, ret_is_float, arg_free_mask: Vec<bool>)
    entries: HashMap<i64, (Value, bool, Vec<bool>)>,
}

/// F3: Global fallback store for asynchronous/off-thread callbacks.
/// When C stores a callback function pointer and invokes it after the
/// synchronous FFI call returns, the thread-local context has been cleared.
/// This global store keeps closures alive so the trampoline can still find
/// them. Entries persist until explicitly deregistered via
/// `mimi_callback_deregister` or until process exit.
static CALLBACK_GLOBAL_STORE: std::sync::LazyLock<Mutex<HashMap<i64, (Value, bool, Vec<bool>)>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// Holds borrow guards alive during a synchronous FFI C call.
/// Each guard variant pairs the lock guard (dropped first) with the `Arc`
/// that keeps the underlying data alive (dropped second).
/// The `Arc` is stored AFTER the guard so that on drop, the guard is
/// released first (unlocking the RwLock) before the Arc potentially frees it.
enum FfiGuard {
    Read(std::sync::RwLockReadGuard<'static, Value>, Arc<RwLock<Value>>),
    Write(std::sync::RwLockWriteGuard<'static, Value>, Arc<RwLock<Value>>),
    /// A libffi closure (dynamic C-compatible function pointer) that must
    /// remain alive for the duration of the C call, plus its boxed userdata.
    CallbackClosure {
        closure: Box<libffi::middle::Closure<'static>>,
        userdata: Box<i64>,
    },
}

/// RAII guard that tracks shared handles created during an FFI call and
/// releases them from SHARED_TABLE on drop (all exit paths).
struct FfiSharedGuard {
    table: &'static crate::ffi::runtime::SharedHandleTable,
    handles: Vec<i64>,
}

impl FfiSharedGuard {
    fn new() -> Self {
        Self {
            table: &crate::ffi::runtime::SHARED_TABLE,
            handles: Vec::new(),
        }
    }

    fn register(&mut self, handle_id: i64) {
        self.handles.push(handle_id);
    }
}

impl Drop for FfiSharedGuard {
    fn drop(&mut self) {
        for id in &self.handles {
            let _ = self.table.release(*id);
        }
    }
}

/// Safe wrapper around `libc::free` for use as a `free_callback`.
fn callback_free_callback(ptr: *mut std::ffi::c_void) {
    if !ptr.is_null() {
        // SAFETY: ptr is a non-null pointer previously obtained from libc::malloc (checked above).
        unsafe { libc::free(ptr); }
    }
}

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

// F5: Guard lifetime extension: FfiGuard stores Arc<RwLock<Value>> after the guard
// so that on drop, the guard is released first (unlocking the RwLock) before the
// Arc potentially frees it. The transmute from '_ to 'static is sound because
// the Arc in the enum variant keeps the underlying data alive for the duration
// of the C call.



impl<'a> Interpreter<'a> {
    pub(crate) fn call_extern(
        &mut self,
        extern_func: &ExternFunc,
        contract: &FfiContract,
        args: Vec<Value>,
    ) -> Result<Value, Errno> {
        // Stage 2 wrapper layer: validate and convert arguments according to the
        // FFI contract before loading any shared library.  This keeps the
        // interpreter FFI path aligned with the codegen wrapper path.
        if contract.args.len() != args.len() {
            return Err(Errno::Generic(format!(
                "FFI wrapper: extern function '{}' expects {} arguments, got {}",
                extern_func.name,
                contract.args.len(),
                args.len()
            )));
        }

        // Stage 4: Check precondition (requires) before the C call
        if self.verify_ffi {
            if let Some(requires_expr) = &contract.requires {
                let result = self.eval_expr(requires_expr);
                match result {
                    Ok(Value::Bool(true)) => { /* precondition holds */ }
                    Ok(Value::Bool(false)) => {
                        return Err(Errno::Generic(format!(
                            "FFI contract violation: precondition of '{}' failed",
                            extern_func.name
                        )));
                    }
                    Ok(other) => {
                        return Err(Errno::Generic(format!(
                            "FFI contract error: precondition of '{}' must evaluate to bool, got {}",
                            extern_func.name, other
                        )));
                    }
                    Err(e) => {
                        return Err(Errno::Generic(format!(
                            "FFI contract error: failed to evaluate precondition of '{}': {}",
                            extern_func.name, e
                        )));
                    }
                }
            }
        }

        // F7: ABI runtime verification — validate contract completeness and function pointer
        if self.verify_ffi {
            self.verify_extern_abi(extern_func, contract)?;
        }

        let mut c_args: Vec<i64> = Vec::with_capacity(args.len());
        let mut string_guards: Vec<std::ffi::CString> = Vec::new();
        let mut shared_handles: Vec<std::sync::Arc<crate::ffi::runtime::SharedHandle>> = Vec::new();
        let mut ffi_guards: Vec<FfiGuard> = Vec::new();
        let mut shared_guard = FfiSharedGuard::new();
        let mut shared_dedup: HashMap<*const (), i64> = HashMap::new();
        let mut callback_ids: Vec<i64> = Vec::new();
        for (arg, arg_contract) in args.iter().zip(&contract.args) {
            let c_arg = self.value_to_ffi_arg(
                arg,
                arg_contract,
                &mut string_guards,
                &mut shared_handles,
                &mut ffi_guards,
                &mut shared_guard,
                &mut shared_dedup,
                &mut callback_ids,
            )?;
            c_args.push(c_arg);
        }

        let lib_path = std::env::var("MIMI_FFI_LIB")
            .map_err(|_| Errno::Generic(
                "MIMI_FFI_LIB environment variable not set for extern function call.\n\
                 Set MIMI_FFI_LIB to the path of the shared library containing the extern function.\n\
                 Example: MIMI_FFI_LIB=/path/to/libfoo.so cargo run".to_string()
            ))?;

        // Load library if not already loaded
        let lib_idx = if let Some(idx) = self.loaded_libs.iter().position(|(path, _)| path == &lib_path) {
            idx
        } else {
            // SAFETY: libloading::Library::new loads a shared library via FFI; the path is guaranteed valid by environment variable check above.
            unsafe {
                let lib = libloading::Library::new(&lib_path)
                    .map_err(|e| Errno::Generic(format!("failed to load library '{}': {}", lib_path, e)))?;
                self.loaded_libs.push((lib_path.clone(), lib));
                self.loaded_libs.len() - 1
            }
        };

        let func_name = extern_func.name.clone();

        // Use libffi CIF for correct ABI handling (proper register routing for float/GP args)
        let result = {
            // Clear errno before call to avoid stale errno
            if contract.check_errno {
                unsafe { *libc::__errno_location() = 0; }
            }

            // Build libffi type descriptors for arguments
            let mut cif_arg_types: Vec<FfiType> = Vec::with_capacity(contract.args.len());
            for arg_contract in &contract.args {
                match arg_contract {
                    FfiArgContract::Float => cif_arg_types.push(FfiType::f64()),
                    FfiArgContract::Callback { .. } => cif_arg_types.push(FfiType::pointer()),
                    _ => cif_arg_types.push(FfiType::i64()),
                }
            }

            // Build libffi type descriptor for return value
            let cif_ret_type = match &contract.ret {
                FfiRetContract::Unit => FfiType::void(),
                FfiRetContract::Float => FfiType::f64(),
                FfiRetContract::String | FfiRetContract::StringOwned | FfiRetContract::Json => FfiType::pointer(),
                _ => FfiType::i64(),
            };

            let cif = Cif::new(cif_arg_types.into_iter(), cif_ret_type);

            // Prepare typed arguments for libffi call
            let mut typed_storage: Vec<Box<dyn std::any::Any>> = Vec::with_capacity(contract.args.len());
            let mut ffi_args: Vec<libffi::middle::Arg> = Vec::with_capacity(contract.args.len());

            for (i, (arg_val, arg_contract)) in args.iter().zip(&contract.args).enumerate() {
                match arg_contract {
                    FfiArgContract::Float => {
                        let f = match arg_val {
                            Value::Float(f) => *f,
                            Value::Int(n) => *n as f64,
                            _ => return Err(Errno::Generic("FFI contract violation: expected float or int".to_string())),
                        };
                        typed_storage.push(Box::new(f));
                        let last = typed_storage.last().ok_or_else(|| "FFI call: typed_storage is empty after push (impossible)".to_string())?;
                        let ptr = last.downcast_ref::<f64>()
                            .ok_or_else(|| "FFI call: expected f64 in typed_storage but downcast failed".to_string())?;
                        ffi_args.push(ffi_arg(ptr));
                    }
                    _ => {
                        let v = c_args[i];
                        typed_storage.push(Box::new(v));
                        let last = typed_storage.last().ok_or_else(|| "FFI call: typed_storage is empty after push (impossible)".to_string())?;
                        let ptr = last.downcast_ref::<i64>()
                            .ok_or_else(|| "FFI call: expected i64 in typed_storage but downcast failed".to_string())?;
                        ffi_args.push(ffi_arg(ptr));
                    }
                }
            }

            let lib = &self.loaded_libs[lib_idx].1;
            // Get the function pointer as a raw address for libffi
            let raw_fn: libloading::Symbol<*mut std::ffi::c_void> = unsafe {
                lib.get(func_name.as_bytes())
                    .map_err(|e| format!("failed to find symbol '{}': {}", func_name, e))?
            };
            let fn_ptr = *raw_fn;
            let code_ptr = CodePtr(fn_ptr);

            // F8: Set up thread-local callback context if any callback contracts exist
            let has_callbacks = contract.args.iter().any(|a| matches!(a, FfiArgContract::Callback { .. }));
            let mut prev_ctx: Option<FfiCallbackCtx> = None;
            if has_callbacks {
                // Save the previous context to handle nested FFI calls correctly.
                // If an FFI callback invokes another FFI call on the same thread,
                // the old context is restored after the inner call completes.
                prev_ctx = Some(FFI_CALLBACK_CTX.with(|c| {
                    let ctx = c.borrow();
                    FfiCallbackCtx {
                        interp: ctx.interp,
                        entries: ctx.entries.clone(),
                    }
                }));
                // SAFETY: self is a mutable reference that lives for the duration of
                // the synchronous C call. The C call may invoke callbacks on the same
                // thread, which will read this context.
                let interp_ptr: *const Interpreter<'_> = self;
                // SAFETY: The interpreter outlives the synchronous C call.
                // The C call runs on the same thread and callbacks only execute
                // during the C function's execution, which is within the scope
                // of `self`.
                let static_ptr = interp_ptr as *const Interpreter<'static>;
                FFI_CALLBACK_CTX.with(|c| {
                    let mut ctx = c.borrow_mut();
                    ctx.interp = static_ptr;
                });
            }

            // Call via libffi with correct ABI and crash protection
            let call_result = if self.verify_ffi {
                self.call_ffi_with_fork_isolation(&cif, code_ptr, &ffi_args, &contract.ret)
            } else {
                self.call_ffi_direct(&cif, code_ptr, &ffi_args, &contract.ret)
            };

            // F8: Clear thread-local callback context after the synchronous call.
            // F3: Remove from CALLBACK_GLOBAL_STORE for synchronous-only callbacks
            // (the common case). Async/off-thread callbacks that store the function
            // pointer will find it via the global store's remaining entries; those
            // are cleaned up via explicit mimi_callback_deregister or process exit.
            if has_callbacks {
                // Collect callback IDs that were added for THIS call
                let ids: Vec<i64> = callback_ids.iter().copied().collect();
                // Remove from global store (sync callbacks don't need persistence)
                {
                    let mut global = CALLBACK_GLOBAL_STORE.lock().unwrap_or_else(|e| e.into_inner());
                    for id in &ids {
                        global.remove(id);
                    }
                }
                // Also remove from CALLBACK_TABLE since these were sync-only
                for id in &ids {
                    CALLBACK_TABLE.remove(*id);
                }
                // Restore the previous context (for nested FFI calls).
                // If there's no saved context (top-level call), clear entirely.
                if let Some(prev) = prev_ctx.take() {
                    FFI_CALLBACK_CTX.with(|c| {
                        let mut ctx = c.borrow_mut();
                        ctx.interp = prev.interp;
                        ctx.entries = prev.entries;
                    });
                } else {
                    FFI_CALLBACK_CTX.with(|c| {
                        let mut ctx = c.borrow_mut();
                        ctx.interp = std::ptr::null();
                        ctx.entries.clear();
                    });
                }
            }

            call_result?
        };

        // Priority 2: Capture errno after C call if enabled
        let errno_value = if contract.check_errno {
            // SAFETY: libc::__errno_location returns a valid pointer to thread-local errno; dereferencing it is safe after an FFI call.
            Some(unsafe { *libc::__errno_location() })
        } else {
            None
        };

        let return_value = self.ffi_ret_to_value(result, &contract.ret)?;

        // Stage 4: Check postcondition (ensures) after the C call
        if self.verify_ffi {
            if let Some(ensures_expr) = &contract.ensures {
                // Bind 'result' to the return value for ensures evaluation
                // by temporarily injecting it into the current scope
                self.push_scope();
                self.env.last_mut().ok_or_else(|| Errno::Generic("FFI call: no scope after push (impossible)".to_string()))?.insert("result".to_string(), return_value.clone());
                let eval_result = self.eval_expr(ensures_expr);
                self.pop_scope();
                match eval_result {
                    Ok(Value::Bool(true)) => { /* postcondition holds */ }
                    Ok(Value::Bool(false)) => {
                        return Err(Errno::Generic(format!(
                            "FFI contract violation: postcondition of '{}' failed",
                            extern_func.name
                        )));
                    }
                    Ok(other) => {
                        return Err(Errno::Generic(format!(
                            "FFI contract error: postcondition of '{}' must evaluate to bool, got {}",
                            extern_func.name, other
                        )));
                    }
                    Err(e) => {
                        return Err(Errno::Generic(format!(
                            "FFI contract error: failed to evaluate postcondition of '{}': {}",
                            extern_func.name, e
                        )));
                    }
                }
            }
        }

        // Priority 2: Map errno to structured Errno if enabled
        if let Some(errno) = errno_value {
            if errno != 0 {
                return Err(Errno::from_code(errno));
            }
        }

        Ok(return_value)
    }

    /// Convert a single Mimi value into a C ABI argument according to the
    /// argument's FFI contract.
    fn value_to_ffi_arg(
        &self,
        arg: &Value,
        contract: &FfiArgContract,
        string_guards: &mut Vec<std::ffi::CString>,
        shared_handles: &mut Vec<std::sync::Arc<crate::ffi::runtime::SharedHandle>>,
        ffi_guards: &mut Vec<FfiGuard>,
        shared_guard: &mut FfiSharedGuard,
        shared_dedup: &mut HashMap<*const (), i64>,
        callback_ids: &mut Vec<i64>,
    ) -> Result<i64, Errno> {
        match contract {
            FfiArgContract::Int => match arg {
                Value::Int(n) => Ok(*n),
                Value::Bool(b) => Ok(*b as i64),
                other => Err(Errno::Generic(format!(
                    "FFI wrapper: expected scalar integer/bool argument, found {}",
                    other
                ))),
            },
            FfiArgContract::Float => match arg {
                Value::Float(f) => Ok(f.to_bits() as i64),
                Value::Int(n) => Ok((*n as f64).to_bits() as i64),
                other => Err(Errno::Generic(format!(
                    "FFI wrapper: expected f64 argument, found {}",
                    other
                ))),
            },
            FfiArgContract::StringBorrow => match arg {
                Value::String(s) => {
                    let c_str = std::ffi::CString::new(s.as_str())
                        .map_err(|e| Errno::Generic(format!("failed to convert string to C string: {}", e)))?;
                    let ptr = c_str.as_ptr() as i64;
                    string_guards.push(c_str); // keep the CString alive during the C call
                    Ok(ptr)
                }
                other => Err(Errno::Generic(format!(
                    "FFI wrapper: expected string argument, found {}",
                    other
                ))),
            },
            FfiArgContract::StringTransfer => match arg {
                Value::String(s) => {
                    // Transfer ownership: strip NUL bytes then create a CString that C must free
                    let sanitized: String = s.as_str().chars().filter(|&c| c != '\0').collect();
                    let c_str = std::ffi::CString::new(sanitized)
                        .map_err(|e| Errno::Generic(format!("failed to convert string to C string: {}", e)))?;
                    // Convert to raw pointer - C is now responsible for freeing
                    let ptr = c_str.into_raw() as i64;
                    Ok(ptr)
                }
                other => Err(Errno::Generic(format!(
                    "FFI wrapper: expected string argument for ownership transfer, found {}",
                    other
                ))),
            },
            FfiArgContract::Cap(mode) => match arg {
                Value::Cap(names) => {
                    let cap_name = names.first().unwrap_or(&String::new()).clone();
                    match mode {
                        CapMode::Move => {
                            // Register as a consumed cap (move semantics)
                            let cap_id = CAP_TABLE.register(&cap_name);
                            CAP_TABLE.consume(cap_id, &cap_name);
                            Ok(cap_id)
                        }
                        CapMode::Borrow => {
                            // Register as a non-consumed cap (borrow semantics)
                            Ok(CAP_TABLE.register(&cap_name))
                        }
                    }
                }
                other => Err(Errno::Generic(format!(
                    "FFI safety: expected cap argument, found {}",
                    other
                ))),
            },
            FfiArgContract::Json => {
                // Serialize the Mimi value to JSON and pass as a C string
                let json_str = self.value_to_json(arg)?;
                let json_text = serde_json::to_string(&json_str)
                    .map_err(|e| Errno::Generic(format!("FFI: failed to serialize value to JSON: {}", e)))?;
                let c_str = std::ffi::CString::new(json_text)
                    .map_err(|e| Errno::Generic(format!("FFI: failed to convert JSON string to C string: {}", e)))?;
                let ptr = c_str.as_ptr() as i64;
                string_guards.push(c_str);
                Ok(ptr)
            }
            FfiArgContract::Unsupported(ty) => {
                Err(self.unsupported_ffi_arg_error(arg, ty))
            }
            FfiArgContract::Callback { param_types, ret_type } => {
                self.value_to_ffi_callback(arg, param_types, ret_type, string_guards, shared_handles, ffi_guards, callback_ids)
            }
            FfiArgContract::RawPtr(_) => match arg {
                // *T: immutable raw pointer
                Value::Shared(arc) => {
                    let arc_ptr = Arc::as_ptr(arc) as *const ();
                    if let Some(&existing_id) = shared_dedup.get(&arc_ptr) {
                        if let Some(handle) = SHARED_TABLE.get(existing_id) {
                            shared_handles.push(handle.clone());
                            let guard = handle.borrow();
                            let ptr = &*guard as *const Value as *const () as i64;
                            ffi_guards.push(FfiGuard::Read(unsafe {
                                std::mem::transmute::<std::sync::RwLockReadGuard<'_, Value>, std::sync::RwLockReadGuard<'static, Value>>(guard)
                            }, Arc::clone(arc)));
                            Ok(ptr)
                        } else {
                            Err(Errno::Generic("FFI wrapper: shared handle missing from table during raw ptr dedup".to_string()))
                        }
                    } else {
                        let handle_id = SHARED_TABLE.create(Arc::clone(arc));
                        shared_dedup.insert(arc_ptr, handle_id);
                        shared_guard.register(handle_id);
                        if let Some(handle) = SHARED_TABLE.get(handle_id) {
                            shared_handles.push(handle.clone());
                            let guard = handle.borrow();
                            let ptr = &*guard as *const Value as *const () as i64;
                            ffi_guards.push(FfiGuard::Read(unsafe {
                                std::mem::transmute::<std::sync::RwLockReadGuard<'_, Value>, std::sync::RwLockReadGuard<'static, Value>>(guard)
                            }, Arc::clone(arc)));
                            Ok(ptr)
                        } else {
                            Err(Errno::Generic("FFI wrapper: failed to create shared handle for raw pointer".to_string()))
                        }
                    }
                }
                Value::Ref(rc) => {
                    let guard = rc.read().map_err(|e| Errno::Generic(format!("read lock failed: {}", e)))?;
                    let ptr = &*guard as *const Value as *const () as i64;
                    // SAFETY: (F5) We hold a clone of the `Arc<RwLock<Value>>` alongside
                    // the guard in `FfiGuard::Read`, so the `Arc` keeps the data alive
                    // for the entire duration of the C call.
                    ffi_guards.push(FfiGuard::Read(unsafe {
                        std::mem::transmute::<std::sync::RwLockReadGuard<'_, Value>, std::sync::RwLockReadGuard<'static, Value>>(guard)
                    }, Arc::clone(rc)));
                    Ok(ptr)
                }
                Value::Int(n) => Ok(*n),
                other => Err(Errno::Generic(format!(
                    "FFI wrapper: raw pointer argument must be a shared value, reference, or opaque handle, found {}",
                    other
                ))),
            },
            FfiArgContract::RawPtrMut(_) => match arg {
                // *mut T: mutable raw pointer
                Value::Shared(arc) => {
                    let arc_ptr = Arc::as_ptr(arc) as *const ();
                    if let Some(&existing_id) = shared_dedup.get(&arc_ptr) {
                        if let Some(handle) = SHARED_TABLE.get(existing_id) {
                            shared_handles.push(handle.clone());
                            let mut guard = handle.borrow_mut();
                            let ptr = &mut *guard as *mut Value as *mut () as i64;
                            ffi_guards.push(FfiGuard::Write(unsafe {
                                std::mem::transmute::<std::sync::RwLockWriteGuard<'_, Value>, std::sync::RwLockWriteGuard<'static, Value>>(guard)
                            }, Arc::clone(arc)));
                            Ok(ptr)
                        } else {
                            Err(Errno::Generic("FFI wrapper: shared handle missing from table during raw ptr mut dedup".to_string()))
                        }
                    } else {
                        let handle_id = SHARED_TABLE.create(Arc::clone(arc));
                        shared_dedup.insert(arc_ptr, handle_id);
                        shared_guard.register(handle_id);
                        if let Some(handle) = SHARED_TABLE.get(handle_id) {
                            shared_handles.push(handle.clone());
                            let mut guard = handle.borrow_mut();
                            let ptr = &mut *guard as *mut Value as *mut () as i64;
                            ffi_guards.push(FfiGuard::Write(unsafe {
                                std::mem::transmute::<std::sync::RwLockWriteGuard<'_, Value>, std::sync::RwLockWriteGuard<'static, Value>>(guard)
                            }, Arc::clone(arc)));
                            Ok(ptr)
                        } else {
                            Err(Errno::Generic("FFI wrapper: failed to create shared handle for mutable raw pointer".to_string()))
                        }
                    }
                }
                Value::RefMut(rc) => {
                    let mut guard = rc.write().map_err(|e| Errno::Generic(format!("write lock failed: {}", e)))?;
                    let ptr = &mut *guard as *mut Value as *mut () as i64;
                    // SAFETY: (F5) We hold a clone of the `Arc<RwLock<Value>>` alongside
                    // the guard in `FfiGuard::Write`, so the `Arc` keeps the data alive.
                    ffi_guards.push(FfiGuard::Write(unsafe {
                        std::mem::transmute::<std::sync::RwLockWriteGuard<'_, Value>, std::sync::RwLockWriteGuard<'static, Value>>(guard)
                    }, Arc::clone(rc)));
                    Ok(ptr)
                }
                Value::Int(n) => Ok(*n),
                other => Err(Errno::Generic(format!(
                    "FFI wrapper: mutable raw pointer argument must be a shared value, mutable reference, or opaque handle, found {}",
                    other
                ))),
            },
            FfiArgContract::CShared(_) => match arg {
                // c_shared T: create a handle in SHARED_TABLE and return the handle ID
                Value::Shared(arc) => {
                    let arc_ptr = Arc::as_ptr(arc) as *const ();
                    if let Some(&existing_id) = shared_dedup.get(&arc_ptr) {
                        Ok(existing_id)
                    } else {
                        let handle_id = SHARED_TABLE.create(Arc::clone(arc));
                        shared_dedup.insert(arc_ptr, handle_id);
                        shared_guard.register(handle_id);
                        Ok(handle_id)
                    }
                }
                Value::LocalShared(rc) => {
                    // Clone the inner value into an Arc<RwLock> for SharedHandle.
                    // The original local_shared retains its local refcount; the FFI
                    // side gets an independent shared copy via the handle table.
                    let handle_id = {
                        let value = rc.0.borrow().clone();
                        let arc = Arc::new(RwLock::new(value));
                        SHARED_TABLE.create(arc)
                    };
                    shared_guard.register(handle_id);
                    Ok(handle_id)
                }
                Value::Int(n) => {
                    // Already an opaque handle (from previous conversion)
                    Ok(*n)
                }
                other => Err(Errno::Generic(format!(
                    "FFI wrapper: c_shared argument must be a shared value or opaque handle, found {}",
                    other
                ))),
            },
            FfiArgContract::CBorrow(_) => match arg {
                // c_borrow T: create a handle and return a pointer to the inner value
                Value::Shared(arc) => {
                    let arc_ptr = Arc::as_ptr(arc) as *const ();
                    if let Some(&existing_id) = shared_dedup.get(&arc_ptr) {
                        if let Some(handle) = SHARED_TABLE.get(existing_id) {
                            shared_handles.push(handle.clone());
                            let guard = handle.borrow();
                            let ptr = &*guard as *const Value as *const () as i64;
                            ffi_guards.push(FfiGuard::Read(unsafe {
                                std::mem::transmute::<std::sync::RwLockReadGuard<'_, Value>, std::sync::RwLockReadGuard<'static, Value>>(guard)
                            }, Arc::clone(arc)));
                            Ok(ptr)
                        } else {
                            Err(Errno::Generic("FFI wrapper: shared handle missing from table during c_borrow dedup".to_string()))
                        }
                    } else {
                        let handle_id = SHARED_TABLE.create(Arc::clone(arc));
                        shared_dedup.insert(arc_ptr, handle_id);
                        shared_guard.register(handle_id);
                        if let Some(handle) = SHARED_TABLE.get(handle_id) {
                            shared_handles.push(handle.clone());
                            let guard = handle.borrow();
                            let ptr = &*guard as *const Value as *const () as i64;
                            ffi_guards.push(FfiGuard::Read(unsafe {
                                std::mem::transmute::<std::sync::RwLockReadGuard<'_, Value>, std::sync::RwLockReadGuard<'static, Value>>(guard)
                            }, Arc::clone(arc)));
                            Ok(ptr)
                        } else {
                            Err(Errno::Generic("FFI wrapper: failed to create shared handle for c_borrow".to_string()))
                        }
                    }
                }
                Value::Ref(rc) => {
                    let guard = rc.read().map_err(|e| Errno::Generic(format!("read lock failed: {}", e)))?;
                    let ptr = &*guard as *const Value as *const () as i64;
                    // SAFETY: (F5) We hold a clone of the `Arc<RwLock<Value>>` alongside
                    // the guard in `FfiGuard::Read`, so the `Arc` keeps the data alive.
                    ffi_guards.push(FfiGuard::Read(unsafe {
                        std::mem::transmute::<std::sync::RwLockReadGuard<'_, Value>, std::sync::RwLockReadGuard<'static, Value>>(guard)
                    }, Arc::clone(rc)));
                    Ok(ptr)
                }
                Value::Int(n) => {
                    Ok(*n)
                }
                other => Err(Errno::Generic(format!(
                    "FFI wrapper: c_borrow argument must be a shared value, reference, or opaque handle, found {}",
                    other
                ))),
            },
            FfiArgContract::CBorrowMut(_) => match arg {
                // c_borrow_mut T: create a handle and return a mutable pointer to the inner value
                Value::Shared(arc) => {
                    let arc_ptr = Arc::as_ptr(arc) as *const ();
                    if let Some(&existing_id) = shared_dedup.get(&arc_ptr) {
                        if let Some(handle) = SHARED_TABLE.get(existing_id) {
                            shared_handles.push(handle.clone());
                            let mut guard = handle.borrow_mut();
                            let ptr = &mut *guard as *mut Value as *mut () as i64;
                            ffi_guards.push(FfiGuard::Write(unsafe {
                                std::mem::transmute::<std::sync::RwLockWriteGuard<'_, Value>, std::sync::RwLockWriteGuard<'static, Value>>(guard)
                            }, Arc::clone(arc)));
                            Ok(ptr)
                        } else {
                            Err(Errno::Generic("FFI wrapper: shared handle missing from table during c_borrow_mut dedup".to_string()))
                        }
                    } else {
                        let handle_id = SHARED_TABLE.create(Arc::clone(arc));
                        shared_dedup.insert(arc_ptr, handle_id);
                        shared_guard.register(handle_id);
                        if let Some(handle) = SHARED_TABLE.get(handle_id) {
                            shared_handles.push(handle.clone());
                            let mut guard = handle.borrow_mut();
                            let ptr = &mut *guard as *mut Value as *mut () as i64;
                            ffi_guards.push(FfiGuard::Write(unsafe {
                                std::mem::transmute::<std::sync::RwLockWriteGuard<'_, Value>, std::sync::RwLockWriteGuard<'static, Value>>(guard)
                            }, Arc::clone(arc)));
                            Ok(ptr)
                        } else {
                            Err(Errno::Generic("FFI wrapper: failed to create shared handle for c_borrow_mut".to_string()))
                        }
                    }
                }
                Value::RefMut(rc) => {
                    let mut guard = rc.write().map_err(|e| Errno::Generic(format!("write lock failed: {}", e)))?;
                    let ptr = &mut *guard as *mut Value as *mut () as i64;
                    // SAFETY: (F5) We hold a clone of the `Arc<RwLock<Value>>` alongside
                    // the guard in `FfiGuard::Write`, so the `Arc` keeps the data alive.
                    ffi_guards.push(FfiGuard::Write(unsafe {
                        std::mem::transmute::<std::sync::RwLockWriteGuard<'_, Value>, std::sync::RwLockWriteGuard<'static, Value>>(guard)
                    }, Arc::clone(rc)));
                    Ok(ptr)
                }
                Value::Int(n) => {
                    Ok(*n)
                }
                other => Err(Errno::Generic(format!(
                    "FFI wrapper: c_borrow_mut argument must be a shared value, mutable reference, or opaque handle, found {}",
                    other
                ))),
            },
        }
    }

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
    fn value_to_ffi_callback(
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
                let arg_free_mask: Vec<bool> = param_types
                    .iter()
                    .map(|pt| matches!(pt, Type::Name(n, _) if n == "string")
                        || matches!(pt, Type::RawString)
                        || matches!(pt, Type::CBuffer(_)))
                    .collect();
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

    /// Convert the raw i64 returned by a C function into a Mimi value according
    /// to the return-value contract.
    fn ffi_ret_to_value(&self, result: i64, contract: &FfiRetContract) -> Result<Value, Errno> {
        match contract {
            FfiRetContract::Unit => Ok(Value::Unit),
            FfiRetContract::Int => Ok(Value::Int(result)),
            FfiRetContract::Float => Ok(Value::Float(f64::from_bits(result as u64))),
            FfiRetContract::String => {
                if result == 0 {
                    Ok(Value::String(String::new()))
                } else {
                    // SAFETY: result is a non-null pointer returned by the FFI call.
                    // The FfiRetContract::String contract asserts the C function returns
                    // a valid null-terminated C string (borrowed, Mimi does NOT free).
                    let c_str = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        unsafe { std::ffi::CStr::from_ptr(result as *const i8) }
                    })).map_err(|_| format!(
                        "FFI safety: C function returned invalid string pointer (address {:#x})", result
                    ))?;
                    // F6: Warn once per process about the String leak pitfall.
                    // The warning text is always visible in the source at the
                    // extern declaration site; this runtime reminder helps users
                    // who don't read the doc comment.
                    static STRING_LEAK_WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
                    if !STRING_LEAK_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                        eprintln!(
                            "[mimi] FFI WARNING: extern function returned 'String' (borrowed). \
                             If C allocated this string, it WILL LEAK. Use 'StringOwned' \
                             for C-allocated strings that Mimi should free, or change the \
                             return type to 'raw_string' and free via mimi_string_free_raw."
                        );
                    }
                    Ok(Value::String(c_str.to_string_lossy().into_owned()))
                }
            }
            FfiRetContract::StringOwned => {
                if result == 0 {
                    Ok(Value::String(String::new()))
                } else {
                    // Read the C string (Mimi takes ownership, must free)
                    let c_str = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        unsafe { std::ffi::CStr::from_ptr(result as *const i8) }
                    })).map_err(|_| format!(
                        "FFI safety: C function returned invalid string pointer (address {:#x})", result
                    ))?;
                    let s = c_str.to_string_lossy().into_owned();
                    // Free the C-allocated string
                    unsafe { libc::free(result as *mut libc::c_void); }
                    Ok(Value::String(s))
                }
            }
            FfiRetContract::Json => {
                if result == 0 {
                    Ok(Value::Unit)
                } else {
                    let c_str = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        unsafe { std::ffi::CStr::from_ptr(result as *const i8) }
                    })).map_err(|_| format!(
                        "FFI safety: C function returned invalid JSON string pointer (address {:#x})", result
                    ))?;
                    let json_str = c_str.to_string_lossy();
                    let json_val: serde_json::Value = serde_json::from_str(&json_str)
                        .map_err(|e| format!("FFI: failed to parse JSON return value: {}", e))?;
                    // Free the C-allocated string
                    unsafe { libc::free(result as *mut libc::c_void); }
                    Ok(self.json_to_value(&json_val))
                }
            }
            FfiRetContract::RawPtr(_)
            | FfiRetContract::RawPtrMut(_)
            | FfiRetContract::CShared(_)
            | FfiRetContract::CBorrow(_)
            | FfiRetContract::CBorrowMut(_) => {
                Ok(Value::Int(result))
            }
            FfiRetContract::Unsupported(ty) => Err(Errno::Generic(format!(
                "FFI safety: extern function declared with unsupported return type '{}'",
                ty
            ))),
        }
    }

    /// Produce a Phase-0-compatible error for Mimi values that cannot cross the
    /// C ABI boundary.  Used when an extern declaration bypassed the type
    /// checker (e.g. in tests that call run_source_result directly).
    fn unsupported_ffi_arg_error(&self, arg: &Value, _ty: &str) -> Errno {
        match arg {
            Value::Shared(_) | Value::LocalShared(_) | Value::WeakShared(_) | Value::WeakLocal(_) => {
                Errno::Generic(format!(
                    "FFI safety: cannot pass shared value '{}' directly to extern function. \
                     Use a passport type such as c_shared T or c_borrow T instead.",
                    arg
                ))
            }
            Value::Ref(_) | Value::RefMut(_) => {
                Errno::Generic(format!(
                    "FFI safety: cannot pass borrowed reference '{}' directly to extern function. \
                     Use a passport type such as c_borrow T or c_borrow_mut T instead.",
                    arg
                ))
            }
            Value::Cap(_) => {
                Errno::Generic(
                    "FFI safety: cap cannot be passed directly to extern functions yet. \
                     Cap cross-boundary authentication (via a runtime CapTable) is planned for Phase 3."
                        .to_string()
                )
            }
            Value::Record(_, _) | Value::Variant(_, _) | Value::List(_) | Value::Tuple(_) => {
                Errno::Generic(format!(
                    "FFI safety: unsupported argument type '{}' for extern function call. \
                     Only scalar types (i32/i64/f64/bool) and borrowed strings are allowed. \
                     Complex Mimi values must be converted to passport types (c_shared T, \
                     c_borrow T, c_borrow_mut T, *T, *mut T) before crossing the FFI boundary.",
                    arg
                ))
            }
            other => {
                Errno::Generic(format!(
                    "FFI safety: unsupported argument type '{}' for extern function call. \
                     Only scalar types (i32/i64/f64/bool) and borrowed strings are allowed. \
                     Complex Mimi values must be converted to passport types (c_shared T, \
                     c_borrow T, c_borrow_mut T, *T, *mut T) before crossing the FFI boundary.",
                    other
                ))
            }
        }
    }

    pub(crate) fn value_to_json(&self, v: &Value) -> Result<serde_json::Value, Errno> {
        match v {
            Value::Int(n) => Ok(serde_json::Value::Number((*n).into())),
            Value::Float(f) => {
                let n = serde_json::Number::from_f64(*f)
                    .ok_or_else(|| format!("float {} cannot be represented in JSON", f))?;
                Ok(serde_json::Value::Number(n))
            }
            Value::Bool(b) => Ok(serde_json::Value::Bool(*b)),
            Value::String(s) => Ok(serde_json::Value::String(s.clone())),
            Value::Unit => Ok(serde_json::Value::Null),
            Value::List(items) => {
                let arr: Result<Vec<_>, _> = items.iter().map(|i| self.value_to_json(i)).collect();
                Ok(serde_json::Value::Array(arr?))
            }
            Value::Record(_, fields) => {
                let mut map = serde_json::Map::new();
                for (k, v) in fields {
                    map.insert(k.clone(), self.value_to_json(v)?);
                }
                Ok(serde_json::Value::Object(map))
            }
            Value::Tuple(items) => {
                let arr: Result<Vec<_>, _> = items.iter().map(|i| self.value_to_json(i)).collect();
                Ok(serde_json::Value::Array(arr?))
            }
            Value::Variant(name, payload) => {
                if payload.is_empty() {
                    Ok(serde_json::Value::String(name.clone()))
                } else {
                    let arr: Result<Vec<_>, _> = payload.iter().map(|i| self.value_to_json(i)).collect();
                    let mut map = serde_json::Map::new();
                    map.insert(name.clone(), serde_json::Value::Array(arr?));
                    Ok(serde_json::Value::Object(map))
                }
            }
            _ => Ok(serde_json::Value::String(format!("{}", v))),
        }
    }

    fn json_to_value(&self, jv: &serde_json::Value) -> Value {
        match jv {
            serde_json::Value::Null => Value::Unit,
            serde_json::Value::Bool(b) => Value::Bool(*b),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Value::Int(i)
                } else if let Some(f) = n.as_f64() {
                    Value::Float(f)
                } else {
                    Value::Unit
                }
            }
            serde_json::Value::String(s) => Value::String(s.clone()),
            serde_json::Value::Array(arr) => {
                Value::List(arr.iter().map(|v| self.json_to_value(v)).collect())
            }
            serde_json::Value::Object(map) => {
                let fields: HashMap<String, Value> = map.iter()
                    .map(|(k, v)| (k.clone(), self.json_to_value(v)))
                    .collect();
                Value::Record(None, fields)
            }
        }
    }

    /// F7: Validate extern ABI — checks callback contract validity and
    /// argument count.  Unsupported-type errors are handled separately by
    /// `unsupported_ffi_arg_error` with richer context.
    fn verify_extern_abi(
        &self,
        extern_func: &ExternFunc,
        contract: &FfiContract,
    ) -> Result<(), Errno> {
        for (i, arg_contract) in contract.args.iter().enumerate() {
            if let FfiArgContract::Callback { param_types, .. } = arg_contract {
                if param_types.is_empty() {
                    return Err(Errno::Generic(format!(
                        "FFI safety: callback parameter {} of '{}' has zero parameters",
                        i + 1,
                        extern_func.name
                    )));
                }
            }
        }
        if contract.args.len() != extern_func.params.len() {
            return Err(Errno::Generic(format!(
                "FFI safety: contract has {} args but extern '{}' declares {} params",
                contract.args.len(),
                extern_func.name,
                extern_func.params.len()
            )));
        }
        Ok(())
    }

    /// Call a C function via libffi (raw, standalone — no self access).
    /// Safe to call after fork() since it doesn't touch Rust data structures
    /// beyond the raw pointers passed in.
    unsafe fn call_ffi_raw(
        cif: &Cif,
        code_ptr: CodePtr,
        ffi_args: &[libffi::middle::Arg],
        ret_contract: &FfiRetContract,
    ) -> i64 {
        match ret_contract {
            FfiRetContract::Unit => {
                cif.call::<()>(code_ptr, ffi_args);
                0i64
            }
            FfiRetContract::Float => {
                let val: f64 = cif.call(code_ptr, ffi_args);
                val.to_bits() as i64
            }
            _ => cif.call::<i64>(code_ptr, ffi_args),
        }
    }

    /// Call a C function without crash protection via libffi.
    fn call_ffi_direct(
        &self,
        cif: &Cif,
        code_ptr: CodePtr,
        ffi_args: &[libffi::middle::Arg],
        ret_contract: &FfiRetContract,
    ) -> Result<i64, String> {
        unsafe {
            Ok(Self::call_ffi_raw(cif, code_ptr, ffi_args, ret_contract))
        }
    }

    /// Call a C function with crash isolation via fork().
    /// If the child process crashes (SIGSEGV, SIGBUS, etc.), returns an Err.
    ///
    /// ⚠ SAFETY: fork() is only safe in single-threaded contexts.
    /// The fork lock serializes fork() against concurrent FFI operations,
    /// but async-signal-safety of libffi calls in the child is not guaranteed.
    fn call_ffi_with_fork_isolation(
        &self,
        cif: &Cif,
        code_ptr: CodePtr,
        ffi_args: &[libffi::middle::Arg],
        ret_contract: &FfiRetContract,
    ) -> Result<i64, String> {
        // Acquire fork lock to serialize fork() with other FFI operations.
        // The lock is held across fork and released in parent/child handlers.
        let _guard = ensure_fork_lock().lock().unwrap_or_else(|e| e.into_inner());

        let mut pipe_fds: [std::ffi::c_int; 2] = [0; 2];
        let pipe_ret = unsafe { libc::pipe(pipe_fds.as_mut_ptr()) };
        if pipe_ret != 0 {
            return Err("FFI safety: failed to create pipe for crash isolation".to_string());
        }

        let pid = unsafe { libc::fork() };
        if pid == 0 {
            // CHILD: run the C call via raw trampoline, send result, _exit.
            // The child must NOT touch any Rust stdlib types (Arc, Mutex, etc.)
            // because they may be in an inconsistent state after fork.
            unsafe { libc::close(pipe_fds[0]); }
            // SAFETY: call_ffi_raw is safe to call after fork() because it
            // doesn't touch any Rust data structures beyond the raw CIF/args.
            let result_code = unsafe { Self::call_ffi_raw(cif, code_ptr, ffi_args, ret_contract) };
            unsafe {
                libc::write(pipe_fds[1], &result_code as *const i64 as *const libc::c_void,
                    std::mem::size_of::<i64>());
                libc::close(pipe_fds[1]);
                libc::_exit(0);
            }
        }

        // PARENT
        unsafe { libc::close(pipe_fds[1]); }

        // Set pipe read end to non-blocking so we never deadlock if the
        // child crashes without writing (e.g., panic unwind, SIGKILL before write).
        unsafe {
            let flags = libc::fcntl(pipe_fds[0], libc::F_GETFL, 0);
            if flags >= 0 {
                libc::fcntl(pipe_fds[0], libc::F_SETFL, flags | libc::O_NONBLOCK);
            }
        }

        // F4: poll waitpid with WNOHANG + timeout so a hung C function does not
        // permanently block the Mimi process.
        let ffi_timeout_ms = std::env::var("MIMI_FFI_TIMEOUT_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(30_000);
        let deadline = std::time::Instant::now()
            .checked_add(std::time::Duration::from_millis(ffi_timeout_ms))
            .unwrap_or_else(|| std::time::Instant::now() + std::time::Duration::from_secs(30));

        let mut status: i32 = 0;
        loop {
            let ret = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
            if ret == pid {
                break; // child exited
            }
            if ret == -1 {
                let err = std::io::Error::last_os_error();
                unsafe { libc::close(pipe_fds[0]); }
                return Err(format!("FFI safety: waitpid error: {}", err));
            }
            if std::time::Instant::now() >= deadline {
                // Timeout — kill the child forcefully
                unsafe { libc::kill(pid, libc::SIGKILL); }
                // Reap the zombie
                unsafe { libc::waitpid(pid, &mut status, 0); }
                unsafe { libc::close(pipe_fds[0]); }
                return Err(format!(
                    "FFI safety: C function timed out after {}ms",
                    ffi_timeout_ms,
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        if unsafe { libc::WIFSIGNALED(status) } {
            let sig = unsafe { libc::WTERMSIG(status) };
            let sig_name = match sig {
                6 => "SIGABRT", 11 => "SIGSEGV", 7 => "SIGBUS",
                4 => "SIGILL", 8 => "SIGFPE", _ => "unknown signal",
            };
            unsafe { libc::close(pipe_fds[0]); }
            return Err(format!("FFI safety: C function crashed with {} (signal {})", sig_name, sig));
        }

        let mut result: i64 = 0;
        let nread = unsafe {
            let n = libc::read(pipe_fds[0], &mut result as *mut i64 as *mut libc::c_void,
                std::mem::size_of::<i64>());
            libc::close(pipe_fds[0]);
            n
        };

        if nread <= 0 {
            // Pipe was empty or error — child exited without writing result.
            // This is unexpected (child _exit should only run after write).
            Err("FFI safety: C function exited without producing a result".to_string())
        } else if result == i64::MIN {
            Err("FFI safety: C function returned an error".to_string())
        } else {
            Ok(result)
        }
    }

    pub(crate) fn value_to_debug_string(&self, v: &Value) -> String {
        match v {
            Value::Int(n) => format!("{}", n),
            Value::Float(f) => format!("{}", f),
            Value::Bool(b) => format!("{}", b),
            Value::String(s) => format!("\"{}\"", s),
            Value::Record(type_name, fields) => {
                let name = type_name.as_deref().unwrap_or("Record");
                let fs: Vec<String> = fields.iter()
                    .map(|(k, v)| format!("{}: {}", k, self.value_to_debug_string(v)))
                    .collect();
                format!("{} {{ {} }}", name, fs.join(", "))
            }
            Value::Variant(name, args) => {
                if args.is_empty() {
                    name.clone()
                } else {
                    let as_: Vec<String> = args.iter().map(|a| self.value_to_debug_string(a)).collect();
                    format!("{}({})", name, as_.join(", "))
                }
            }
            Value::List(items) => {
                let is_: Vec<String> = items.iter().map(|i| self.value_to_debug_string(i)).collect();
                format!("[{}]", is_.join(", "))
            }
            Value::Tuple(items) => {
                let ts: Vec<String> = items.iter().map(|i| self.value_to_debug_string(i)).collect();
                format!("({})", ts.join(", "))
            }
            Value::Unit => "unit".to_string(),
            _ => format!("{:?}", v),
        }
    }

    pub(crate) fn values_equal(&self, a: &Value, b: &Value) -> bool {
        // Delegate to the canonical implementation in value.rs to avoid duplication.
        // The canonical version supports more Value variants (Shared, Ref, DynTrait, etc.)
        // and uses relative epsilon for float comparison.
        crate::interp::value::values_equal(a, b)
    }
}

/// Debug formatting for FFI argument contract
fn ffi_arg_contract_to_debug(c: &FfiArgContract) -> String {
    match c {
        FfiArgContract::Int => "i64".to_string(),
        FfiArgContract::Float => "f64".to_string(),
        FfiArgContract::StringBorrow => "const char* (borrowed)".to_string(),
        FfiArgContract::StringTransfer => "char* (transferred)".to_string(),
        FfiArgContract::Cap(m) => format!("cap({})", if *m == CapMode::Move { "move" } else { "borrow" }),
        FfiArgContract::RawPtr(t) => format!("*{:?}", t),
        FfiArgContract::RawPtrMut(t) => format!("*mut {:?}", t),
        FfiArgContract::CShared(t) => format!("c_shared {:?}", t),
        FfiArgContract::CBorrow(t) => format!("c_borrow {:?}", t),
        FfiArgContract::CBorrowMut(t) => format!("c_borrow_mut {:?}", t),
        FfiArgContract::Json => "json (char*)".to_string(),
        FfiArgContract::Callback { param_types, ret_type } => {
            let pts: Vec<String> = param_types.iter().map(|t| format!("{:?}", t)).collect();
            format!("fn({}) -> {:?}", pts.join(", "), ret_type)
        }
        FfiArgContract::Unsupported(t) => format!("unsupported({})", t),
    }
}

/// Debug formatting for FFI return contract
fn ffi_ret_contract_to_debug(c: &FfiRetContract) -> String {
    match c {
        FfiRetContract::Unit => "void".to_string(),
        FfiRetContract::Int => "i64".to_string(),
        FfiRetContract::Float => "f64".to_string(),
        FfiRetContract::String => "char* (borrowed)".to_string(),
        FfiRetContract::StringOwned => "char* (owned)".to_string(),
        FfiRetContract::Json => "json (char*)".to_string(),
        FfiRetContract::RawPtr(t) => format!("*{:?}", t),
        FfiRetContract::RawPtrMut(t) => format!("*mut {:?}", t),
        FfiRetContract::CShared(t) => format!("c_shared {:?}", t),
        FfiRetContract::CBorrow(t) => format!("c_borrow {:?}", t),
        FfiRetContract::CBorrowMut(t) => format!("c_borrow_mut {:?}", t),
        FfiRetContract::Unsupported(t) => format!("unsupported({})", t),
    }
}

// ===================== F6 Callback String-Leak Tests =====================
// Verifies that C-allocated string arguments passed to Mimi callbacks
// are freed after the callback returns.

#[cfg(test)]
mod callback_leak_tests {
    use super::*;
    use crate::ast::Type;

    /// Helper: compute arg_free_mask as done in value_to_ffi_callback.
    fn compute_free_mask(param_types: &[Type]) -> Vec<bool> {
        let mut mask = Vec::new();
        for pt in param_types {
            let should_free = matches!(pt, Type::Name(n, _) if n == "string")
                || matches!(pt, Type::RawString)
                || matches!(pt, Type::CBuffer(_));
            mask.push(should_free);
        }
        mask
    }

    #[test]
    fn test_free_mask_i32_args_no_free() {
        let types = [Type::Name("i32".into(), Vec::new()), Type::Name("i64".into(), Vec::new())];
        assert_eq!(compute_free_mask(&types), [false, false]);
    }

    #[test]
    fn test_free_mask_string_arg_freed() {
        let types = [Type::Name("string".into(), Vec::new())];
        assert_eq!(compute_free_mask(&types), [true]);
    }

    #[test]
    fn test_free_mask_mixed_args() {
        let types = [
            Type::Name("i32".into(), Vec::new()),
            Type::Name("string".into(), Vec::new()),
            Type::Name("f64".into(), Vec::new()),
        ];
        assert_eq!(compute_free_mask(&types), [false, true, false]);
    }

    #[test]
    fn test_free_mask_raw_string() {
        let types = [Type::RawString];
        assert_eq!(compute_free_mask(&types), [true]);
    }

    #[test]
    fn test_free_mask_cbuffer() {
        let types = [Type::CBuffer(Box::new(Type::Name("u8".into(), Vec::new())))];
        assert_eq!(compute_free_mask(&types), [true]);
    }

    #[test]
    fn test_callback_ctx_three_tuple() {
        // Verify FfiCallbackCtx entries store (Value, bool, Vec<bool>).
        let entry: (Value, bool, Vec<bool>) = (Value::Int(0), false, Vec::from([true, false]));
        assert_eq!(entry.2.len(), 2);
        assert!(entry.2[0]);
        assert!(!entry.2[1]);
    }

    #[test]
    fn test_trampoline_frees_null_safe() {
        // Verify libc::free(NULL) is safe (no crash).
        // NULL free is a no-op in C.
        unsafe { libc::free(std::ptr::null_mut()) };
    }
}
