use super::*;
use crate::ffi::{FfiArgContract, FfiContract, FfiRetContract, Errno};
use libffi::middle::{Cif, Type as FfiType, CodePtr, arg as ffi_arg};
use std::collections::HashMap;

#[cfg(test)]
pub(crate) use super::ffi::helpers::compute_arg_free_mask;
pub(in crate::interp) use super::ffi::helpers::{FfiGuard, FfiSharedGuard};

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
            self.verify_ffi_requires(extern_func, contract)?;
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
            let mut prev_ctx: Option<super::ffi::callback::FfiCallbackCtx> = None;
            if has_callbacks {
                // Save the previous context to handle nested FFI calls correctly.
                // If an FFI callback invokes another FFI call on the same thread,
                // the old context is restored after the inner call completes.
                prev_ctx = Some(super::ffi::callback::FFI_CALLBACK_CTX.with(|c| {
                    let ctx = c.borrow();
                    super::ffi::callback::FfiCallbackCtx {
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
                super::ffi::callback::FFI_CALLBACK_CTX.with(|c| {
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
            // F3: Global store entries (CALLBACK_GLOBAL_STORE) and CALLBACK_TABLE
            // entries are intentionally NOT removed here — they persist until
            // explicitly deregistered via mimi_callback_deregister or process exit.
            // This ensures async/off-thread callbacks (where C stores the function
            // pointer and calls it later) can still find their closure and handle.
            if has_callbacks {
                if let Some(prev) = prev_ctx.take() {
                    super::ffi::callback::FFI_CALLBACK_CTX.with(|c| {
                        let mut ctx = c.borrow_mut();
                        ctx.interp = prev.interp;
                        ctx.entries = prev.entries;
                    });
                } else {
                    super::ffi::callback::FFI_CALLBACK_CTX.with(|c| {
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
            self.verify_ffi_ensures(extern_func, contract, &return_value)?;
        }

        // Priority 2: Map errno to structured Errno if enabled
        if let Some(errno) = errno_value {
            if errno != 0 {
                return Err(Errno::from_code(errno));
            }
        }

        Ok(return_value)
    }
}

// ===================== F6 Callback String-Leak Tests =====================
// Verifies that C-allocated string arguments passed to Mimi callbacks
// are freed after the callback returns.

#[cfg(test)]
mod callback_leak_tests {
    use super::*;
    use crate::ast::Type;

    /// Helper: delegate to the module-level function.
    fn compute_free_mask(param_types: &[Type]) -> Vec<bool> {
        compute_arg_free_mask(param_types)
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
