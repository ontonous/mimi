use super::super::*;
use super::helpers::{FfiGuard, FfiSharedGuard};
use crate::ast::*;
use crate::ffi::{FfiArgContract, FfiRetContract, CAP_TABLE, SHARED_TABLE, Errno};
use std::collections::HashMap;
use std::ffi::CString;
use std::sync::Arc;

impl<'a> Interpreter<'a> {
    /// Convert a single Mimi value into a C ABI argument according to the
    /// argument's FFI contract.
    pub(in crate::interp) fn value_to_ffi_arg(
        &self,
        arg: &Value,
        contract: &FfiArgContract,
        string_guards: &mut Vec<CString>,
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
                    let c_str = CString::new(s.as_str())
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
                    let c_str = CString::new(sanitized)
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
                let c_str = CString::new(json_text)
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
                    // the guard in `FfiGuard::Read`, so the `Arc` keeps the underlying data alive
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
                    // the guard in `FfiGuard::Read`, so the `Arc` keeps the underlying data alive.
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
                    // the guard in `FfiGuard::Write`, so the `Arc` keeps the underlying data alive.
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

    /// Convert the raw i64 returned by a C function into a Mimi value according
    /// to the return-value contract.
    pub(in crate::interp) fn ffi_ret_to_value(&self, result: i64, contract: &FfiRetContract) -> Result<Value, Errno> {
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
    pub(in crate::interp) fn unsupported_ffi_arg_error(&self, arg: &Value, _ty: &str) -> Errno {
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

    pub(in crate::interp) fn json_to_value(&self, jv: &serde_json::Value) -> Value {
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
}
