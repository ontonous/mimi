use super::*;
use crate::ast::{Field, TypeAttribute, TypeDefKind};
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
        // Buffer for struct-by-value marshalled data; kept alive during the C call.
        // SAFETY (F-17): Each inner Vec<u8> owns its heap allocation independently.
        // Pushing to the outer Vec only moves the inner Vec handle (ptr+len+cap),
        // NOT its heap data. Raw data pointers taken via as_ptr() remain stable
        // across outer Vec reallocation.
        let mut struct_buffers: Vec<Vec<u8>> = Vec::new();
        for (arg, arg_contract) in args.iter().zip(&contract.args) {
            match arg_contract {
                FfiArgContract::StructByValue(_) => {
                    c_args.push(0); // placeholder; actual marshalling in arg-prep loop
                }
                _ => {
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
            }
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
            // Uses platform-specific errno location (libc crate exports
            // __errno_location on Linux, __error on macOS).
            // Capturing side reads errno via std::io::Error::last_os_error().
            if contract.check_errno {
                #[cfg(any(target_os = "linux", target_os = "android"))]
                unsafe { *libc::__errno_location() = 0; }
                #[cfg(target_os = "macos")]
                unsafe { *libc::__error() = 0; }
            }

            // Build libffi type descriptors for arguments
            let mut cif_arg_types: Vec<FfiType> = Vec::with_capacity(contract.args.len());
            for arg_contract in &contract.args {
                match arg_contract {
                    FfiArgContract::Float => cif_arg_types.push(FfiType::f64()),
                    FfiArgContract::Callback { .. } => cif_arg_types.push(FfiType::pointer()),
                    FfiArgContract::StructByValue(type_name) => {
                        let fields = self.lookup_struct_fields(type_name)
                            .map_err(|e| Errno::Generic(e))?;
                        let field_types: Result<Vec<FfiType>, String> = fields.iter()
                            .map(|f| self.ffi_type_from_mimi_type(&f.ty))
                            .collect();
                        let field_types = field_types.map_err(|e| Errno::Generic(e))?;
                        cif_arg_types.push(FfiType::structure(field_types));
                    }
                    _ => cif_arg_types.push(FfiType::i64()),
                }
            }

            // Build libffi type descriptor for return value
            // Pre-compute struct-by-value return buffer size if needed.
            let mut struct_ret_size: Option<usize> = None;
            let cif_ret_type = match &contract.ret {
                FfiRetContract::Unit => FfiType::void(),
                FfiRetContract::Float => FfiType::f64(),
                FfiRetContract::String | FfiRetContract::StringOwned | FfiRetContract::Json => FfiType::pointer(),
                FfiRetContract::StructByValue(type_name) => {
                    let fields = self.lookup_struct_fields(type_name)
                        .map_err(|e| Errno::Generic(e))?;
                    let (total_size, _) = self.struct_size_align(&fields)
                        .map_err(|e| Errno::Generic(e))?;
                    struct_ret_size = Some(total_size);
                    let field_types: Result<Vec<FfiType>, String> = fields.iter()
                        .map(|f| self.ffi_type_from_mimi_type(&f.ty))
                        .collect();
                    let field_types = field_types.map_err(|e| Errno::Generic(e))?;
                    FfiType::structure(field_types)
                }
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
                    FfiArgContract::StructByValue(type_name) => {
                        let fields = self.lookup_struct_fields(type_name)
                            .map_err(|e| Errno::Generic(e))?;
                        let buffer = self.marshall_record_to_buffer(arg_val, &fields)
                            .map_err(|e| Errno::Generic(e))?;
                        // SAFETY: Buffer heap data is stable (Vec only moves handle).
                        // Arg::new stores a raw data pointer; struct_buffers keeps
                        // the buffer alive for the synchronous C call.
                        let data_ptr = buffer.as_ptr() as *mut std::ffi::c_void;
                        // Create Arg pointing to the first byte of buffer data.
                        let arg = unsafe { ffi_arg(&*data_ptr) };
                        struct_buffers.push(buffer);
                        ffi_args.push(arg);
                        typed_storage.push(Box::new(0i64)); // placeholder
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
            // Struct-by-value return uses custom rvalue buffer; other paths use scalar return.
            let call_result: Result<i64, String> = if let Some(buf_size) = struct_ret_size {
                // Allocate zeroed buffer for the struct return value.
                let mut ret_buf = vec![0u8; buf_size];
                let rvalue = ret_buf.as_mut_ptr() as *mut std::ffi::c_void;
                // F-16: Apply crash protection to struct-by-value returns.
                if self.verify_ffi {
                    self.call_ffi_with_fork_isolation_struct(&cif, code_ptr, &ffi_args, &mut ret_buf)?;
                } else if extern_func.no_panic {
                    self.call_ffi_no_panic_struct(&cif, code_ptr, &ffi_args, rvalue)?;
                } else {
                    // SAFETY: call_ffi_raw_struct uses the low-level ffi_call API
                    // with a caller-provided return buffer. rvalue points to a valid
                    // ret_buf allocation.
                    unsafe { Self::call_ffi_raw_struct(&cif, code_ptr, &ffi_args, rvalue); }
                }
                struct_buffers.push(ret_buf);
                Ok(0i64) // placeholder; actual result read from buffer below
            } else if self.verify_ffi {
                self.call_ffi_with_fork_isolation(&cif, code_ptr, &ffi_args, &contract.ret)
            } else if extern_func.no_panic {
                self.call_ffi_no_panic(&cif, code_ptr, &ffi_args, &contract.ret)
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

        // Decode the return value: i64 for scalar/ptr returns; buffer for struct returns.
        let return_value = if let FfiRetContract::StructByValue(type_name) = &contract.ret {
            // Read the last buffer pushed to struct_buffers (the struct return buffer).
            if let Some(ret_buf) = struct_buffers.pop() {
                let fields = self.lookup_struct_fields(type_name)
                    .map_err(|e| Errno::Generic(e))?;
                self.unmarshall_buffer_to_record(&ret_buf, &fields)?
            } else {
                return Err(Errno::Generic(
                    "FFI wrapper: struct return buffer missing".to_string()
                ));
            }
        } else {
            self.ffi_ret_to_value(result, &contract.ret)?
        };

        // Priority 2: Capture errno after C call if enabled
        // Uses std::io::Error::last_os_error() which calls the platform-specific
        // errno accessor (__errno_location on glibc, __error on macOS, GetLastError
        // on Windows), avoiding a direct dependency on glibc internal symbols.
        let errno_value = if contract.check_errno {
            Some(std::io::Error::last_os_error().raw_os_error().unwrap_or(0))
        } else {
            None
        };

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

    // ── struct-by-value helpers ──────────────────────────────────────────

    /// Look up the record fields for a StructByValue type name.
    pub(in crate::interp) fn lookup_struct_fields(
        &self,
        type_name: &str,
    ) -> Result<Vec<Field>, String> {
        let td = self.type_defs.get(type_name).ok_or_else(|| {
            format!("StructByValue: type '{}' not found in type_defs", type_name)
        })?;
        match &td.kind {
            TypeDefKind::Record(fields) => Ok(fields.clone()),
            _ => Err(format!(
                "StructByValue: type '{}' is not a record (kind={:?})",
                type_name, td.kind
            )),
        }
    }

    /// Convert a Mimi `Type` to a libffi `Type` for struct field layout.
    /// Only supports types valid in #[repr(C)] records: scalars and nested
    /// #[repr(C)] records.
    pub(in crate::interp) fn ffi_type_from_mimi_type(&self, ty: &crate::ast::Type) -> Result<FfiType, String> {
        match ty {
            crate::ast::Type::Name(name, _) => match name.as_str() {
                "i32" => Ok(FfiType::i32()),
                "i64" => Ok(FfiType::i64()),
                "f64" => Ok(FfiType::f64()),
                "bool" => Ok(FfiType::u8()),
                other => {
                    // Check for nested #[repr(C)] record
                    if let Some(td) = self.type_defs.get(other) {
                        if td.attributes.contains(&TypeAttribute::ReprC) {
                            if let TypeDefKind::Record(fields) = &td.kind {
                                let field_types: Result<Vec<FfiType>, String> = fields
                                    .iter()
                                    .map(|f| self.ffi_type_from_mimi_type(&f.ty))
                                    .collect();
                                return Ok(FfiType::structure(field_types?));
                            }
                        }
                    }
                    Err(format!(
                        "StructByValue: unsupported field type '{}' in #[repr(C)] record",
                        name
                    ))
                }
            },
            _ => Err(format!(
                "StructByValue: unsupported type '{:?}' in #[repr(C)] record",
                ty
            )),
        }
    }

    /// Compute the size and alignment of a Mimi type in #[repr(C)] layout.
    fn mimi_type_size_align(&self, ty: &crate::ast::Type) -> Result<(usize, usize), String> {
        match ty {
            crate::ast::Type::Name(name, _) => match name.as_str() {
                "i32" => Ok((4, 4)),
                "i64" => Ok((8, 8)),
                "f64" => Ok((8, 8)),
                "bool" => Ok((1, 1)),
                other => {
                    if let Some(td) = self.type_defs.get(other) {
                        if td.attributes.contains(&TypeAttribute::ReprC) {
                            if let TypeDefKind::Record(fields) = &td.kind {
                                return self.struct_size_align(fields);
                            }
                        }
                    }
                    Err(format!(
                        "StructByValue: unsupported field type '{}' in #[repr(C)] record",
                        name
                    ))
                }
            },
            _ => Err(format!(
                "StructByValue: unsupported type '{:?}' in #[repr(C)] record",
                ty
            )),
        }
    }

    /// Compute the total size and alignment of a #[repr(C)] struct from its fields.
    fn struct_size_align(&self, fields: &[Field]) -> Result<(usize, usize), String> {
        let mut current_offset = 0usize;
        let mut max_align = 1usize;
        for field in fields {
            let (size, align) = self.mimi_type_size_align(&field.ty)?;
            let aligned = (current_offset + align - 1) & !(align - 1);
            current_offset = aligned + size;
            max_align = max_align.max(align);
        }
        // Round up to max alignment
        let total = (current_offset + max_align - 1) & !(max_align - 1);
        Ok((total, max_align))
    }

    /// Marshal a Mimi `Value::Record` into a byte buffer matching #[repr(C)]
    /// layout. The field types are used to compute offsets and sizes.
    fn marshall_record_to_buffer(
        &self,
        val: &Value,
        fields: &[Field],
    ) -> Result<Vec<u8>, String> {
        let field_vals = match val {
            Value::Record(_, map) => map,
            _ => return Err(format!("StructByValue: expected Record, got {}", val)),
        };

        // Build field offset/size info
        let mut offsets: Vec<usize> = Vec::with_capacity(fields.len());
        let mut sizes: Vec<usize> = Vec::with_capacity(fields.len());
        let mut current_offset = 0usize;
        for field in fields {
            let (size, align) = self.mimi_type_size_align(&field.ty)?;
            let aligned = (current_offset + align - 1) & !(align - 1);
            offsets.push(aligned);
            sizes.push(size);
            current_offset = aligned + size;
        }

        let (total_size, _) = self.struct_size_align(fields)?;
        let mut buf = vec![0u8; total_size];

        for (i, field) in fields.iter().enumerate() {
            let offset = offsets[i];
            let fv = field_vals.get(&field.name).ok_or_else(|| {
                format!("StructByValue: field '{}' missing in record value", field.name)
            })?;
            self.write_field_to_buf(fv, &field.ty, &mut buf, offset)?;
        }

        Ok(buf)
    }

    /// Write a single Mimi value into a byte buffer at the given offset,
    /// using the C ABI scalar layout (little-endian).
    fn write_field_to_buf(&self, val: &Value, ty: &crate::ast::Type, buf: &mut [u8], offset: usize) -> Result<(), String> {
        let type_name = match ty {
            crate::ast::Type::Name(n, _) => n.as_str(),
            _ => return Err(format!("StructByValue: cannot write field of type {:?}", ty)),
        };
        match type_name {
            "i32" => {
                let v = match val {
                    Value::Int(n) => *n as i32,
                    Value::Bool(b) => *b as i32,
                    _ => return Err(format!("StructByValue: expected i32, got {}", val)),
                };
                let bytes = v.to_le_bytes();
                buf[offset..offset+4].copy_from_slice(&bytes);
            }
            "i64" => {
                let v = match val {
                    Value::Int(n) => *n,
                    _ => return Err(format!("StructByValue: expected i64, got {}", val)),
                };
                let bytes = v.to_le_bytes();
                buf[offset..offset+8].copy_from_slice(&bytes);
            }
            "f64" => {
                let v = match val {
                    Value::Float(f) => *f,
                    Value::Int(n) => *n as f64,
                    _ => return Err(format!("StructByValue: expected f64, got {}", val)),
                };
                let bytes = v.to_bits().to_le_bytes();
                buf[offset..offset+8].copy_from_slice(&bytes);
            }
            "bool" => {
                let v = match val {
                    Value::Bool(b) => *b as u8,
                    Value::Int(n) => {
                        if *n == 0 { 0u8 } else { 1u8 }
                    }
                    _ => return Err(format!("StructByValue: expected bool, got {}", val)),
                };
                buf[offset] = v;
            }
            other => {
                // Check for nested #[repr(C)] record
                if let Some(td) = self.type_defs.get(other) {
                    if td.attributes.contains(&TypeAttribute::ReprC) {
                        if let TypeDefKind::Record(fields) = &td.kind {
                            if let Value::Record(_, map) = val {
                                // Build a sub-record from the map for recursive marshalling
                                let sub_val = Value::Record(None, map.clone());
                                let sub_buf = self.marshall_record_to_buffer(&sub_val, fields)?;
                                let len = sub_buf.len();
                                if offset + len <= buf.len() {
                                    buf[offset..offset+len].copy_from_slice(&sub_buf);
                                    return Ok(());
                                }
                            }
                        }
                    }
                }
                return Err(format!(
                    "StructByValue: unsupported field type '{}' in record",
                    other
                ));
            }
        }
        Ok(())
    }

    /// Unmarshal a byte buffer (from C struct return) back to a Mimi Record.
    fn unmarshall_buffer_to_record(
        &self,
        buf: &[u8],
        fields: &[Field],
    ) -> Result<Value, Errno> {
        let mut field_vals = std::collections::HashMap::new();
        let mut current_offset = 0usize;
        for field in fields {
            let (size, align) = self.mimi_type_size_align(&field.ty)
                .map_err(|e| Errno::Generic(e))?;
            let aligned = (current_offset + align - 1) & !(align - 1);
            let val = self.read_field_from_buf(buf, &field.ty, aligned)?;
            field_vals.insert(field.name.clone(), val);
            current_offset = aligned + size;
        }
        Ok(Value::Record(None, field_vals))
    }

    /// Read a single field value from a byte buffer at the given offset.
    fn read_field_from_buf(&self, buf: &[u8], ty: &crate::ast::Type, offset: usize) -> Result<Value, Errno> {
        let type_name = match ty {
            crate::ast::Type::Name(n, _) => n.as_str(),
            _ => return Err(Errno::Generic(format!(
                "StructByValue: cannot read field of type {:?}", ty
            ))),
        };
        match type_name {
            "i32" => {
                let mut bytes = [0u8; 4];
                bytes.copy_from_slice(&buf[offset..offset+4]);
                Ok(Value::Int(i32::from_le_bytes(bytes) as i64))
            }
            "i64" => {
                let mut bytes = [0u8; 8];
                bytes.copy_from_slice(&buf[offset..offset+8]);
                Ok(Value::Int(i64::from_le_bytes(bytes)))
            }
            "f64" => {
                let mut bytes = [0u8; 8];
                bytes.copy_from_slice(&buf[offset..offset+8]);
                Ok(Value::Float(f64::from_le_bytes(bytes)))
            }
            "bool" => {
                Ok(Value::Bool(buf[offset] != 0))
            }
            other => {
                // Nested #[repr(C)] record
                if let Some(td) = self.type_defs.get(other) {
                    if td.attributes.contains(&TypeAttribute::ReprC) {
                        if let TypeDefKind::Record(fields) = &td.kind {
                            return self.unmarshall_buffer_to_record(buf, fields);
                        }
                    }
                }
                Err(Errno::Generic(format!(
                    "StructByValue: unsupported field type '{}' in record return", other
                )))
            }
        }
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
