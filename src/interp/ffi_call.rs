use super::*;
use crate::ffi::{FfiArgContract, FfiContract, FfiRetContract, CAP_TABLE, SHARED_TABLE};

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

        let mut c_args: Vec<i64> = Vec::with_capacity(args.len());
        let mut _string_guards: Vec<std::ffi::CString> = Vec::new();
        let mut _shared_handles: Vec<std::sync::Arc<crate::ffi::runtime::SharedHandle>> = Vec::new();
        let mut _borrow_guards_read: Vec<Box<dyn std::any::Any>> = Vec::new();
        let mut _borrow_guards_write: Vec<Box<dyn std::any::Any>> = Vec::new();
        for (arg, arg_contract) in args.iter().zip(&contract.args) {
            let c_arg = self.value_to_ffi_arg(
                arg,
                arg_contract,
                &mut _string_guards,
                &mut _shared_handles,
                &mut _borrow_guards_read,
                &mut _borrow_guards_write,
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
            unsafe {
                let lib = libloading::Library::new(&lib_path)
                    .map_err(|e| format!("failed to load library '{}': {}", lib_path, e))?;
                self.loaded_libs.push(lib);
                self.loaded_libs.len() - 1
            }
        };

        let func_name = extern_func.name.clone();

        // Call the function via libloading
        let result = unsafe {
            // Clear errno before call to avoid stale errno
            if contract.check_errno {
                *libc::__errno_location() = 0;
            }
            let lib = &self.loaded_libs[lib_idx];
            type CFunc = unsafe extern "C" fn(i64, i64, i64, i64, i64, i64, i64, i64) -> i64;
            let symbol: libloading::Symbol<CFunc> = lib.get(func_name.as_bytes())
                .map_err(|e| format!("failed to find symbol '{}': {}", func_name, e))?;

            // Call with up to 8 args (zeroed if fewer)
            let mut raw_args = [0i64; 8];
            for (i, &a) in c_args.iter().enumerate().take(8) {
                raw_args[i] = a;
            }
            symbol(raw_args[0], raw_args[1], raw_args[2], raw_args[3],
                   raw_args[4], raw_args[5], raw_args[6], raw_args[7])
        };

        // Priority 2: Capture errno after C call if enabled
        let errno_value = if contract.check_errno {
            Some(unsafe { *libc::__errno_location() })
        } else {
            None
        };

        let return_value = self.ffi_ret_to_value(result, &contract.ret)?;

        // Stage 4: Check postcondition (ensures) after the C call
        if self.verify_ffi {
            if let Some(ensures_expr) = &contract.ensures {
                // Bind 'result' to the return value for ensures evaluation
                // Note: The eval_expr method doesn't support scope binding directly,
                // so we use a simpler approach - just evaluate the expression
                // A more complete implementation would inject 'result' into the scope
                let eval_result = self.eval_expr(ensures_expr);
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
        _shared_handles: &mut Vec<std::sync::Arc<crate::ffi::runtime::SharedHandle>>,
        _borrow_guards_read: &mut Vec<Box<dyn std::any::Any>>,
        _borrow_guards_write: &mut Vec<Box<dyn std::any::Any>>,
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
            FfiArgContract::Unsupported(ty) => {
                // Runtime fallback for declarations that bypass the type checker.
                // Preserve the old Phase 0 error messages for the common unsafe
                // Mimi value categories.
                Err(self.unsupported_ffi_arg_error(arg, ty))
            }
            FfiArgContract::RawPtr(_) => match arg {
                // *T: immutable raw pointer
                Value::Shared(arc) => {
                    // Create a handle to keep the shared value alive
                    let handle_id = SHARED_TABLE.create(Arc::clone(arc));
                    // Get a pointer to the inner value
                    if let Some(handle) = SHARED_TABLE.get(handle_id) {
                        let ptr = handle.as_ptr() as *const () as i64;
                        Ok(ptr)
                    } else {
                        Err("FFI wrapper: failed to create shared handle for raw pointer".to_string())
                    }
                }
                Value::Ref(rc) => {
                    let borrow = rc.borrow();
                    let ptr = &*borrow as *const Value as *const () as i64;
                    std::mem::forget(borrow);
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
                    // Create a handle to keep the shared value alive
                    let handle_id = SHARED_TABLE.create(Arc::clone(arc));
                    // Get a mutable pointer to the inner value
                    if let Some(handle) = SHARED_TABLE.get(handle_id) {
                        let ptr = handle.as_mut_ptr() as *mut () as i64;
                        Ok(ptr)
                    } else {
                        Err("FFI wrapper: failed to create shared handle for mutable raw pointer".to_string())
                    }
                }
                Value::RefMut(rc) => {
                    let mut borrow = rc.borrow_mut();
                    let ptr = &mut *borrow as *mut Value as *mut () as i64;
                    std::mem::forget(borrow);
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
                    // Create a handle to keep the shared value alive
                    let handle_id = SHARED_TABLE.create(Arc::clone(arc));
                    // Get a pointer to the inner value
                    if let Some(handle) = SHARED_TABLE.get(handle_id) {
                        let ptr = handle.as_ptr() as *const () as i64;
                        Ok(ptr)
                    } else {
                        Err("FFI wrapper: failed to create shared handle for c_borrow".to_string())
                    }
                }
                Value::Ref(rc) => {
                    let borrow = rc.borrow();
                    let ptr = &*borrow as *const Value as *const () as i64;
                    std::mem::forget(borrow);
                    Ok(ptr)
                }
                Value::Int(n) => {
                    // Already an opaque handle
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
                    // Create a handle to keep the shared value alive
                    let handle_id = SHARED_TABLE.create(Arc::clone(arc));
                    // Get a mutable pointer to the inner value
                    if let Some(handle) = SHARED_TABLE.get(handle_id) {
                        let ptr = handle.as_mut_ptr() as *mut () as i64;
                        Ok(ptr)
                    } else {
                        Err("FFI wrapper: failed to create shared handle for c_borrow_mut".to_string())
                    }
                }
                Value::RefMut(rc) => {
                    let mut borrow = rc.borrow_mut();
                    let ptr = &mut *borrow as *mut Value as *mut () as i64;
                    std::mem::forget(borrow);
                    Ok(ptr)
                }
                Value::Int(n) => {
                    // Already an opaque handle
                    Ok(*n)
                }
                other => Err(format!(
                    "FFI wrapper: c_borrow_mut argument must be a shared value, mutable reference, or opaque handle, found {}",
                    other
                )),
            },
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
                    let c_str = unsafe { std::ffi::CStr::from_ptr(result as *const i8) };
                    Ok(Value::String(c_str.to_string_lossy().into_owned()))
                }
            }
            FfiRetContract::RawPtr(_)
            | FfiRetContract::RawPtrMut(_)
            | FfiRetContract::CShared(_)
            | FfiRetContract::CBorrow(_)
            | FfiRetContract::CBorrowMut(_) => {
                // Passport pointers/handles are returned as opaque integers for now.
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
