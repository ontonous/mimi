use super::*;
use crate::ffi::{FfiArgContract, FfiContract, FfiRetContract, CAP_TABLE, SHARED_TABLE, CALLBACK_TABLE};
use libffi::middle::{Cif, Type as FfiType, CodePtr, arg as ffi_arg};
use libffi::low::{self as ffi_low};
use std::cell::RefCell;
use std::collections::HashMap;

// F8: Thread-local context for synchronous callback invocation.
// Set before each FFI call that involves callbacks, cleared after.
// Maps callback_id -> (Mimi closure, ret_is_float).
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
    entries: HashMap<i64, (Value, bool)>,
}

/// Holds borrow guards alive during a synchronous FFI C call.
/// Stores the concrete guard type so it can be held across 'static boundaries.
enum FfiGuard {
    Read(std::sync::RwLockReadGuard<'static, Value>),
    Write(std::sync::RwLockWriteGuard<'static, Value>),
    RefRead(std::cell::Ref<'static, Value>),
    RefWrite(std::cell::RefMut<'static, Value>),
    /// A libffi closure (dynamic C-compatible function pointer) that must
    /// remain alive for the duration of the C call, plus its boxed userdata.
    CallbackClosure {
        closure: Box<libffi::middle::Closure<'static>>,
        userdata: Box<i64>,
    },
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
    let entry = FFI_CALLBACK_CTX.with(|c| {
        let ctx = c.borrow();
        ctx.entries.get(&callback_id).cloned()
    });
    let Some((closure, ret_is_float)) = entry else {
        *result = 0;
        return;
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
                    _ => 0,
                };
            }
        }
        Err(_) => {
            *result = i64::MIN;
        }
    }
}

/// Extend a RwLockReadGuard's lifetime to 'static.
/// SAFETY: Caller must ensure the underlying RwLock outlives this guard.
unsafe fn extend_guard_read<'a>(g: std::sync::RwLockReadGuard<'a, Value>) -> std::sync::RwLockReadGuard<'static, Value> {
    std::mem::transmute(g)
}

/// Extend a RwLockWriteGuard's lifetime to 'static.
/// SAFETY: Caller must ensure the underlying RwLock outlives this guard.
unsafe fn extend_guard_write<'a>(g: std::sync::RwLockWriteGuard<'a, Value>) -> std::sync::RwLockWriteGuard<'static, Value> {
    std::mem::transmute(g)
}

/// Extend a Ref's lifetime to 'static.
/// SAFETY: Caller must ensure the underlying RefCell outlives this guard.
unsafe fn extend_ref<'a>(g: std::cell::Ref<'a, Value>) -> std::cell::Ref<'static, Value> {
    std::mem::transmute(g)
}

/// Extend a RefMut's lifetime to 'static.
/// SAFETY: Caller must ensure the underlying RefCell outlives this guard.
unsafe fn extend_ref_mut<'a>(g: std::cell::RefMut<'a, Value>) -> std::cell::RefMut<'static, Value> {
    std::mem::transmute(g)
}

impl<'a> Interpreter<'a> {
    pub(crate) fn call_extern(
        &mut self,
        extern_func: &ExternFunc,
        contract: &FfiContract,
        args: Vec<Value>,
    ) -> Result<Value, String> {
        // Stage 2 wrapper layer: validate and convert arguments according to the
        // FFI contract before loading any shared library.  This keeps the
        // interpreter FFI path aligned with the codegen wrapper path.
        if contract.args.len() != args.len() {
            return Err(format!(
                "FFI wrapper: extern function '{}' expects {} arguments, got {}",
                extern_func.name,
                contract.args.len(),
                args.len()
            ));
        }

        // Stage 4: Check precondition (requires) before the C call
        if self.verify_ffi {
            if let Some(requires_expr) = &contract.requires {
                let result = self.eval_expr(requires_expr);
                match result {
                    Ok(Value::Bool(true)) => { /* precondition holds */ }
                    Ok(Value::Bool(false)) => {
                        return Err(format!(
                            "FFI contract violation: precondition of '{}' failed",
                            extern_func.name
                        ));
                    }
                    Ok(other) => {
                        return Err(format!(
                            "FFI contract error: precondition of '{}' must evaluate to bool, got {}",
                            extern_func.name, other
                        ));
                    }
                    Err(e) => {
                        return Err(format!(
                            "FFI contract error: failed to evaluate precondition of '{}': {}",
                            extern_func.name, e
                        ));
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
        for (arg, arg_contract) in args.iter().zip(&contract.args) {
            let c_arg = self.value_to_ffi_arg(
                arg,
                arg_contract,
                &mut string_guards,
                &mut shared_handles,
                &mut ffi_guards,
            )?;
            c_args.push(c_arg);
        }

        let lib_path = std::env::var("MIMI_FFI_LIB")
            .map_err(|_| "MIMI_FFI_LIB environment variable not set for extern function call".to_string())?;

        // Load library if not already loaded
        let lib_idx = if let Some(idx) = self.loaded_libs.iter().position(|l| {
            format!("{:?}", l) == format!("Library({})", lib_path)
        }) {
            idx
        } else {
            // Safety: libloading::Library::new loads a shared library via FFI; the path is guaranteed valid by environment variable check above.
            unsafe {
                let lib = libloading::Library::new(&lib_path)
                    .map_err(|e| format!("failed to load library '{}': {}", lib_path, e))?;
                self.loaded_libs.push(lib);
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
                            _ => unreachable!("FFI contract ensures float arg is float or int"),
                        };
                        typed_storage.push(Box::new(f));
                        let ptr = typed_storage.last().unwrap().downcast_ref::<f64>().unwrap();
                        ffi_args.push(ffi_arg(ptr));
                    }
                    _ => {
                        let v = c_args[i];
                        typed_storage.push(Box::new(v));
                        let ptr = typed_storage.last().unwrap().downcast_ref::<i64>().unwrap();
                        ffi_args.push(ffi_arg(ptr));
                    }
                }
            }

            let lib = &self.loaded_libs[lib_idx];
            // Get the function pointer as a raw address for libffi
            let raw_fn: libloading::Symbol<*mut std::ffi::c_void> = unsafe {
                lib.get(func_name.as_bytes())
                    .map_err(|e| format!("failed to find symbol '{}': {}", func_name, e))?
            };
            let fn_ptr = *raw_fn;
            let code_ptr = CodePtr(fn_ptr);

            // F8: Set up thread-local callback context if any callback contracts exist
            let has_callbacks = contract.args.iter().any(|a| matches!(a, FfiArgContract::Callback { .. }));
            if has_callbacks {
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
                    c.borrow_mut().interp = static_ptr;
                });
            }

            // Call via libffi with correct ABI and crash protection
            let call_result = if self.verify_ffi {
                self.call_ffi_with_fork_isolation(&cif, code_ptr, &ffi_args, &contract.ret)
            } else {
                self.call_ffi_direct(&cif, code_ptr, &ffi_args, &contract.ret)
            };

            // F8: Clear thread-local callback context
            if has_callbacks {
                FFI_CALLBACK_CTX.with(|c| {
                    let mut ctx = c.borrow_mut();
                    ctx.interp = std::ptr::null();
                    ctx.entries.clear();
                });
            }

            call_result?
        };

        // Priority 2: Capture errno after C call if enabled
        let errno_value = if contract.check_errno {
            // Safety: libc::__errno_location returns a valid pointer to thread-local errno; dereferencing it is safe after an FFI call.
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
                self.env.last_mut().unwrap().insert("result".to_string(), return_value.clone());
                let eval_result = self.eval_expr(ensures_expr);
                self.pop_scope();
                match eval_result {
                    Ok(Value::Bool(true)) => { /* postcondition holds */ }
                    Ok(Value::Bool(false)) => {
                        return Err(format!(
                            "FFI contract violation: postcondition of '{}' failed",
                            extern_func.name
                        ));
                    }
                    Ok(other) => {
                        return Err(format!(
                            "FFI contract error: postcondition of '{}' must evaluate to bool, got {}",
                            extern_func.name, other
                        ));
                    }
                    Err(e) => {
                        return Err(format!(
                            "FFI contract error: failed to evaluate postcondition of '{}': {}",
                            extern_func.name, e
                        ));
                    }
                }
            }
        }

        // Priority 2: Map errno to Result if enabled
        if let Some(errno) = errno_value {
            if errno != 0 {
                // Create an Err result with errno information
                let errno_name = match errno {
                    1 => "EPERM",
                    2 => "ENOENT",
                    3 => "ESRCH",
                    4 => "EINTR",
                    5 => "EIO",
                    6 => "ENXIO",
                    7 => "E2BIG",
                    8 => "ENOEXEC",
                    9 => "EBADF",
                    10 => "ECHILD",
                    11 => "EAGAIN",
                    12 => "ENOMEM",
                    13 => "EACCES",
                    14 => "EFAULT",
                    16 => "EBUSY",
                    17 => "EEXIST",
                    18 => "EXDEV",
                    19 => "ENODEV",
                    20 => "ENOTDIR",
                    21 => "EISDIR",
                    22 => "EINVAL",
                    23 => "ENFILE",
                    24 => "EMFILE",
                    25 => "ENOTTY",
                    26 => "ETXTBSY",
                    27 => "EFBIG",
                    28 => "ENOSPC",
                    29 => "ESPIPE",
                    30 => "EROFS",
                    32 => "EPIPE",
                    34 => "EDOM",
                    36 => "ERANGE",
                    38 => "ENOSYS",
                    39 => "ENOTEMPTY",
                    97 => "EAFNOSUPPORT",
                    98 => "EADDRINUSE",
                    99 => "EADDRNOTAVAIL",
                    101 => "ENETUNREACH",
                    104 => "ECONNRESET",
                    110 => "ETIMEDOUT",
                    111 => "ECONNREFUSED",
                    113 => "EHOSTUNREACH",
                    _ => "UNKNOWN",
                };
                return Err(format!(
                    "FFI errno: {} (code {})",
                    errno_name, errno
                ));
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
    ) -> Result<i64, String> {
        match contract {
            FfiArgContract::Int => match arg {
                Value::Int(n) => Ok(*n),
                Value::Bool(b) => Ok(*b as i64),
                other => Err(format!(
                    "FFI wrapper: expected scalar integer/bool argument, found {}",
                    other
                )),
            },
            FfiArgContract::Float => match arg {
                Value::Float(f) => Ok(f.to_bits() as i64),
                Value::Int(n) => Ok((*n as f64).to_bits() as i64),
                other => Err(format!(
                    "FFI wrapper: expected f64 argument, found {}",
                    other
                )),
            },
            FfiArgContract::StringBorrow => match arg {
                Value::String(s) => {
                    let c_str = std::ffi::CString::new(s.as_str())
                        .map_err(|e| format!("failed to convert string to C string: {}", e))?;
                    let ptr = c_str.as_ptr() as i64;
                    string_guards.push(c_str); // keep the CString alive during the C call
                    Ok(ptr)
                }
                other => Err(format!(
                    "FFI wrapper: expected string argument, found {}",
                    other
                )),
            },
            FfiArgContract::StringTransfer => match arg {
                Value::String(s) => {
                    // Transfer ownership: create a CString that C must free
                    let c_str = std::ffi::CString::new(s.as_str())
                        .map_err(|e| format!("failed to convert string to C string: {}", e))?;
                    // Convert to raw pointer - C is now responsible for freeing
                    let ptr = c_str.into_raw() as i64;
                    Ok(ptr)
                }
                other => Err(format!(
                    "FFI wrapper: expected string argument for ownership transfer, found {}",
                    other
                )),
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
                other => Err(format!(
                    "FFI safety: expected cap argument, found {}",
                    other
                )),
            },
            FfiArgContract::Json => {
                // Serialize the Mimi value to JSON and pass as a C string
                let json_str = self.value_to_json(arg)?;
                let json_text = serde_json::to_string(&json_str)
                    .map_err(|e| format!("FFI: failed to serialize value to JSON: {}", e))?;
                let c_str = std::ffi::CString::new(json_text)
                    .map_err(|e| format!("FFI: failed to convert JSON string to C string: {}", e))?;
                let ptr = c_str.as_ptr() as i64;
                string_guards.push(c_str);
                Ok(ptr)
            }
            FfiArgContract::Unsupported(ty) => {
                Err(self.unsupported_ffi_arg_error(arg, ty))
            }
            FfiArgContract::Callback { param_types, ret_type } => {
                self.value_to_ffi_callback(arg, param_types, ret_type, string_guards, shared_handles, ffi_guards)
            }
            FfiArgContract::RawPtr(_) => match arg {
                // *T: immutable raw pointer
                Value::Shared(arc) => {
                    let handle_id = SHARED_TABLE.create(Arc::clone(arc));
                    if let Some(handle) = SHARED_TABLE.get(handle_id) {
                        shared_handles.push(handle.clone());
                        let guard = handle.borrow();
                        let ptr = &*guard as *const Value as *const () as i64;
                        ffi_guards.push(FfiGuard::Read(unsafe { extend_guard_read(guard) }));
                        Ok(ptr)
                    } else {
                        Err("FFI wrapper: failed to create shared handle for raw pointer".to_string())
                    }
                }
                Value::Ref(rc) => {
                    let borrow = rc.borrow();
                    let ptr = &*borrow as *const Value as *const () as i64;
                    ffi_guards.push(FfiGuard::RefRead(unsafe { extend_ref(borrow) }));
                    Ok(ptr)
                }
                Value::Int(n) => Ok(*n),
                other => Err(format!(
                    "FFI wrapper: raw pointer argument must be a shared value, reference, or opaque handle, found {}",
                    other
                )),
            },
            FfiArgContract::RawPtrMut(_) => match arg {
                // *mut T: mutable raw pointer
                Value::Shared(arc) => {
                    let handle_id = SHARED_TABLE.create(Arc::clone(arc));
                    if let Some(handle) = SHARED_TABLE.get(handle_id) {
                        shared_handles.push(handle.clone());
                        let mut guard = handle.borrow_mut();
                        let ptr = &mut *guard as *mut Value as *mut () as i64;
                        ffi_guards.push(FfiGuard::Write(unsafe { extend_guard_write(guard) }));
                        Ok(ptr)
                    } else {
                        Err("FFI wrapper: failed to create shared handle for mutable raw pointer".to_string())
                    }
                }
                Value::RefMut(rc) => {
                    let mut borrow = rc.borrow_mut();
                    let ptr = &mut *borrow as *mut Value as *mut () as i64;
                    ffi_guards.push(FfiGuard::RefWrite(unsafe { extend_ref_mut(borrow) }));
                    Ok(ptr)
                }
                Value::Int(n) => Ok(*n),
                other => Err(format!(
                    "FFI wrapper: mutable raw pointer argument must be a shared value, mutable reference, or opaque handle, found {}",
                    other
                )),
            },
            FfiArgContract::CShared(_) => match arg {
                // c_shared T: create a handle in SHARED_TABLE and return the handle ID
                Value::Shared(arc) => {
                    let handle_id = SHARED_TABLE.create(Arc::clone(arc));
                    Ok(handle_id)
                }
                Value::LocalShared(_rc) => {
                    // Convert LocalShared to Shared for handle creation
                    // Note: This is a limitation - LocalShared cannot be directly used with SharedHandleTable
                    // For now, return an error
                    Err("FFI wrapper: c_shared does not support local_shared values yet. Use shared instead.".to_string())
                }
                Value::Int(n) => {
                    // Already an opaque handle (from previous conversion)
                    Ok(*n)
                }
                other => Err(format!(
                    "FFI wrapper: c_shared argument must be a shared value or opaque handle, found {}",
                    other
                )),
            },
            FfiArgContract::CBorrow(_) => match arg {
                // c_borrow T: create a handle and return a pointer to the inner value
                Value::Shared(arc) => {
                    let handle_id = SHARED_TABLE.create(Arc::clone(arc));
                    if let Some(handle) = SHARED_TABLE.get(handle_id) {
                        shared_handles.push(handle.clone());
                        let guard = handle.borrow();
                        let ptr = &*guard as *const Value as *const () as i64;
                        ffi_guards.push(FfiGuard::Read(unsafe { extend_guard_read(guard) }));
                        Ok(ptr)
                    } else {
                        Err("FFI wrapper: failed to create shared handle for c_borrow".to_string())
                    }
                }
                Value::Ref(rc) => {
                    let borrow = rc.borrow();
                    let ptr = &*borrow as *const Value as *const () as i64;
                    ffi_guards.push(FfiGuard::RefRead(unsafe { extend_ref(borrow) }));
                    Ok(ptr)
                }
                Value::Int(n) => {
                    Ok(*n)
                }
                other => Err(format!(
                    "FFI wrapper: c_borrow argument must be a shared value, reference, or opaque handle, found {}",
                    other
                )),
            },
            FfiArgContract::CBorrowMut(_) => match arg {
                // c_borrow_mut T: create a handle and return a mutable pointer to the inner value
                Value::Shared(arc) => {
                    let handle_id = SHARED_TABLE.create(Arc::clone(arc));
                    if let Some(handle) = SHARED_TABLE.get(handle_id) {
                        shared_handles.push(handle.clone());
                        let mut guard = handle.borrow_mut();
                        let ptr = &mut *guard as *mut Value as *mut () as i64;
                        ffi_guards.push(FfiGuard::Write(unsafe { extend_guard_write(guard) }));
                        Ok(ptr)
                    } else {
                        Err("FFI wrapper: failed to create shared handle for c_borrow_mut".to_string())
                    }
                }
                Value::RefMut(rc) => {
                    let mut borrow = rc.borrow_mut();
                    let ptr = &mut *borrow as *mut Value as *mut () as i64;
                    ffi_guards.push(FfiGuard::RefWrite(unsafe { extend_ref_mut(borrow) }));
                    Ok(ptr)
                }
                Value::Int(n) => {
                    Ok(*n)
                }
                other => Err(format!(
                    "FFI wrapper: c_borrow_mut argument must be a shared value, mutable reference, or opaque handle, found {}",
                    other
                )),
            },
        }
    }

    /// F8: Apply a Mimi closure value to arguments from within a C callback context.
    /// Mirrors `apply_closure` in call.rs but designed for &self usage from a
    /// C trampoline via raw pointer.
    pub(crate) fn apply_closure_ffi(&mut self, closure: &Value, args: Vec<Value>) -> Result<Value, String> {
        match closure {
            Value::Closure { params, body, captured, .. } => {
                if params.len() != args.len() {
                    return Err(format!(
                        "closure expects {} arguments, got {}",
                        params.len(),
                        args.len()
                    ));
                }
                self.push_scope();
                for (n, v) in captured {
                    self.bind(n, v.clone());
                }
                for (param, arg) in params.iter().zip(args) {
                    self.bind(&param.name, arg);
                }
                let result = self.eval_block(body)?;
                self.pop_scope();
                if let Some(val) = self.early_return.take() {
                    return Ok(val);
                }
                Ok(result.unwrap_or(Value::Unit))
            }
            _ => Err(format!("expected a closure, found {}", closure)),
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
    ) -> Result<i64, String> {
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
                    None,
                    Some(Box::new(|_id: i64, _args: &[i64]| -> i64 { 0 })),
                );

                // Store the closure in the thread-local callback context
                FFI_CALLBACK_CTX.with(|c| {
                    let mut ctx = c.borrow_mut();
                    ctx.entries.insert(cb_id, (closure, ret_is_float));
                });

                // Create a libffi Closure that generates a C-compatible function pointer
                // The userdata (callback_id) must outlive the closure.
                // We box the id and store it alongside the closure in FfiGuard.
                let userdata = Box::new(cb_id);
                let cb_ref = &*userdata as &i64 as *const i64;
                // SAFETY: userdata box is leaked and kept alive in FfiGuard
                let cb_ref_static: &'static i64 = unsafe { &*cb_ref };
                let ffi_closure = libffi::middle::Closure::new(
                    cif,
                    mimi_callback_trampoline_fn as ffi_low::Callback<i64, i64>,
                    cb_ref_static,
                );

                let code_ptr = ffi_closure.code_ptr();
                let fn_ptr = code_ptr as *const unsafe extern "C" fn() as *const () as i64;

                // Keep the closure and its userdata alive for the duration of the C call
                ffi_guards.push(FfiGuard::CallbackClosure {
                    closure: Box::new(ffi_closure),
                    userdata,
                });

                Ok(fn_ptr)
            }
            Value::Int(n) => {
                // Already an opaque function pointer (passed through from a previous call)
                Ok(*n)
            }
            other => Err(format!(
                "FFI safety: expected a closure or function pointer for callback parameter, found {}",
                other
            )),
        }
    }

    /// Convert the raw i64 returned by a C function into a Mimi value according
    /// to the return-value contract.
    fn ffi_ret_to_value(&self, result: i64, contract: &FfiRetContract) -> Result<Value, String> {
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
                    // Note: borrowed string - the C side retains ownership.
                    // If the C function allocated this string, it will leak.
                    // Use StringOwned contract variant for C-allocated strings.
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
            FfiRetContract::Unsupported(ty) => Err(format!(
                "FFI safety: extern function declared with unsupported return type '{}'",
                ty
            )),
        }
    }

    /// Produce a Phase-0-compatible error for Mimi values that cannot cross the
    /// C ABI boundary.  Used when an extern declaration bypassed the type
    /// checker (e.g. in tests that call run_source_result directly).
    fn unsupported_ffi_arg_error(&self, arg: &Value, _ty: &str) -> String {
        match arg {
            Value::Shared(_) | Value::LocalShared(_) | Value::WeakShared(_) | Value::WeakLocal(_) => {
                format!(
                    "FFI safety: cannot pass shared value '{}' directly to extern function. \
                     Use a passport type such as c_shared T or c_borrow T instead.",
                    arg
                )
            }
            Value::Ref(_) | Value::RefMut(_) => {
                format!(
                    "FFI safety: cannot pass borrowed reference '{}' directly to extern function. \
                     Use a passport type such as c_borrow T or c_borrow_mut T instead.",
                    arg
                )
            }
            Value::Cap(_) => {
                "FFI safety: cap cannot be passed directly to extern functions yet. \
                 Cap cross-boundary authentication (via a runtime CapTable) is planned for Phase 3."
                    .to_string()
            }
            Value::Record(_, _) | Value::Variant(_, _) | Value::List(_) | Value::Tuple(_) => {
                format!(
                    "FFI safety: unsupported argument type '{}' for extern function call. \
                     Only scalar types (i32/i64/f64/bool) and borrowed strings are allowed. \
                     Complex Mimi values must be converted to passport types (c_shared T, \
                     c_borrow T, c_borrow_mut T, *T, *mut T) before crossing the FFI boundary.",
                    arg
                )
            }
            other => {
                format!(
                    "FFI safety: unsupported argument type '{}' for extern function call. \
                     Only scalar types (i32/i64/f64/bool) and borrowed strings are allowed. \
                     Complex Mimi values must be converted to passport types (c_shared T, \
                     c_borrow T, c_borrow_mut T, *T, *mut T) before crossing the FFI boundary.",
                    other
                )
            }
        }
    }

    pub(crate) fn value_to_json(&self, v: &Value) -> Result<serde_json::Value, String> {
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
    ) -> Result<(), String> {
        for (i, arg_contract) in contract.args.iter().enumerate() {
            if let FfiArgContract::Callback { param_types, .. } = arg_contract {
                if param_types.is_empty() {
                    return Err(format!(
                        "FFI safety: callback parameter {} of '{}' has zero parameters",
                        i + 1,
                        extern_func.name
                    ));
                }
            }
        }
        if contract.args.len() != extern_func.params.len() {
            return Err(format!(
                "FFI safety: contract has {} args but extern '{}' declares {} params",
                contract.args.len(),
                extern_func.name,
                extern_func.params.len()
            ));
        }
        Ok(())
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
            match ret_contract {
                FfiRetContract::Unit => {
                    cif.call::<()>(code_ptr, ffi_args);
                    Ok(0i64)
                }
                FfiRetContract::Float => {
                    let val: f64 = cif.call(code_ptr, ffi_args);
                    Ok(val.to_bits() as i64)
                }
                _ => Ok(cif.call::<i64>(code_ptr, ffi_args)),
            }
        }
    }

    /// Call a C function with crash isolation via fork().
    /// If the child process crashes (SIGSEGV, SIGBUS, etc.), returns an Err.
    fn call_ffi_with_fork_isolation(
        &self,
        cif: &Cif,
        code_ptr: CodePtr,
        ffi_args: &[libffi::middle::Arg],
        ret_contract: &FfiRetContract,
    ) -> Result<i64, String> {
        let mut pipe_fds: [std::ffi::c_int; 2] = [0, 0];
        let pipe_ret = unsafe { libc::pipe(pipe_fds.as_mut_ptr()) };
        if pipe_ret != 0 {
            return Err("FFI safety: failed to create pipe for crash isolation".to_string());
        }

        let pid = unsafe { libc::fork() };
        if pid == 0 {
            // CHILD: run the C call, send result, _exit
            unsafe { libc::close(pipe_fds[0]); }
            let result = self.call_ffi_direct(cif, code_ptr, ffi_args, ret_contract);
            let result_code = match result {
                Ok(val) => val,
                Err(_) => i64::MIN,
            };
            unsafe {
                libc::write(pipe_fds[1], &result_code as *const i64 as *const libc::c_void,
                    std::mem::size_of::<i64>());
                libc::close(pipe_fds[1]);
                libc::_exit(0);
            }
        }

        // PARENT
        unsafe { libc::close(pipe_fds[1]); }
        let mut status: i32 = 0;
        unsafe { libc::waitpid(pid, &mut status, 0); }

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
        unsafe {
            libc::read(pipe_fds[0], &mut result as *mut i64 as *mut libc::c_void,
                std::mem::size_of::<i64>());
            libc::close(pipe_fds[0]);
        }

        if result == i64::MIN {
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
        match (a, b) {
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Unit, Value::Unit) => true,
            (Value::Record(n1, f1), Value::Record(n2, f2)) => {
                if n1 != n2 || f1.len() != f2.len() {
                    return false;
                }
                f1.iter().all(|(k, v)| {
                    if let Some(v2) = f2.get(k) {
                        self.values_equal(v, v2)
                    } else {
                        false
                    }
                })
            }
            (Value::Variant(n1, a1), Value::Variant(n2, a2)) => {
                n1 == n2 && a1.len() == a2.len()
                    && a1.iter().zip(a2.iter()).all(|(a, b)| self.values_equal(a, b))
            }
            (Value::List(a), Value::List(b)) => {
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| self.values_equal(x, y))
            }
            (Value::Tuple(a), Value::Tuple(b)) => {
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| self.values_equal(x, y))
            }
            _ => false,
        }
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
