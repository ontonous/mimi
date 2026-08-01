//! Shared FFI execution runtime for both interpreter backends.
//!
//! Extracted from `ffi_call.rs` / `ffi/convert.rs` / `ffi/call.rs` during
//! 0.33 (Phase D, sprint 0.33.20 "FFI"): the FFI execution context — extern
//! function tables, loaded shared libraries, contract verification — is
//! independent of the interpreter engine, so the Bytecode VM can forward
//! extern calls through the exact same code path as the tree-walker.
//!
//! The only engine-specific piece is callback execution: `FfiClosureRunner`
//! abstracts "run a Mimi closure" so the C trampoline can evaluate closures
//! registered from either backend.

use crate::ast::{
    AstNodeMeta, AstOrigin, ExternFunc, Field, File, Item, Type, TypeAttribute, TypeDef,
    TypeDefKind,
};
use crate::ffi::{
    cap_table_consume, cap_table_register, shared_table_create, shared_table_create_dedup,
    shared_table_get, Errno, FfiArgContract, FfiContract, FfiRetContract,
};
use libffi::middle::{arg as ffi_arg, Cif, CodePtr, Type as FfiType};
use std::collections::HashMap;
use std::ffi::CString;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, RwLock};

use super::ffi::helpers::{ffi_guard_new_read, ffi_guard_new_write, FfiGuard, FfiSharedGuard};
use super::Value;

/// Abstraction over an execution engine that can run a Mimi closure.
///
/// Used by the FFI callback trampoline to evaluate closures registered from
/// either the tree-walker interpreter (`Value::Closure`) or the bytecode VM
/// (`Value::BytecodeClosure`).
pub(in crate::interp) trait FfiClosureRunner {
    /// The program File (for cross-thread callback evaluation).
    fn ffi_file(&self) -> &File;
    /// Apply a Mimi closure value to arguments, from a C trampoline context.
    fn apply_closure_ffi(&mut self, closure: &Value, args: Vec<Value>) -> Result<Value, String>;
    /// Evaluate a contract expression (FFI requires/ensures). `result_binding`
    /// (ensures) binds `result` in a fresh scope.
    fn eval_contract_expr(
        &mut self,
        expr: &crate::ast::Expr,
        result_binding: Option<&Value>,
    ) -> Result<Value, String>;
    /// Optional BytecodeProgram pointer, used for cross-thread BytecodeClosure
    /// evaluation (0.33 Phase D FFI forwarding). Interpreter returns None and
    /// uses the tree-walker cross-thread path instead.
    fn ffi_bytecode_program(
        &self,
    ) -> Option<*const crate::interp::bytecode::instr::BytecodeProgram> {
        None
    }
}

/// FFI execution context shared by both interpreter backends.
pub(crate) struct FfiRuntime {
    /// Extern function declarations: func_name -> ExternFunc.
    pub extern_funcs: HashMap<String, ExternFunc>,
    /// Pre-computed FFI contracts for extern functions.
    pub ffi_contracts: HashMap<String, FfiContract>,
    /// Type definitions for reflection: type_name -> (fields, variants).
    /// Used for #[repr(C)] struct marshalling.
    pub type_defs: HashMap<String, TypeDef>,
    /// Loaded shared libraries: (lib_path, Library handle).
    loaded_libs: Vec<(String, libloading::Library)>,
    /// Whether to verify FFI contracts (requires/ensures) at runtime.
    pub verify_ffi: bool,
    /// Whether to wrap extern calls in the SD-4 signal guard (SIGSEGV/SIGABRT
    /// crash protection). Independent of `verify_ffi` so the bytecode VM can
    /// keep crash protection while contract evaluation is not yet implemented.
    pub signal_guard: bool,
    /// The execution engine driving the current synchronous FFI call.
    ///
    /// Set by the caller (tree-walker `Interpreter` or bytecode `BytecodeVM`)
    /// immediately before `call_extern` and cleared right after. Only valid
    /// during that synchronous call — the same discipline as the thread-local
    /// `FFI_CALLBACK_CTX` pointer (SAFETY analysis in callback.rs).
    runner: Option<*mut (dyn FfiClosureRunner + 'static)>,
}

impl FfiRuntime {
    /// Build the FFI tables from a parsed program file.
    pub(in crate::interp) fn from_file(file: &File) -> Self {
        let mut type_defs = HashMap::new();
        for item in &file.items {
            Self::collect_type_defs(item, &mut type_defs);
        }
        let cap_defs = Self::collect_caps_all(file);
        let mut extern_funcs: HashMap<String, ExternFunc> = HashMap::new();
        let mut ffi_contracts: HashMap<String, FfiContract> = HashMap::new();
        for item in &file.items {
            Self::collect_extern_funcs(
                item,
                &mut extern_funcs,
                &mut ffi_contracts,
                &cap_defs,
                &type_defs,
            );
        }
        FfiRuntime {
            extern_funcs,
            ffi_contracts,
            type_defs,
            loaded_libs: Vec::new(),
            // Matches the historical tree-walker default (Interpreter::new).
            verify_ffi: true,
            signal_guard: true,
            runner: None,
        }
    }

    /// Build FFI tables reusing already-collected tables (tree-walker path).
    pub(in crate::interp) fn from_parts(
        extern_funcs: HashMap<String, ExternFunc>,
        ffi_contracts: HashMap<String, FfiContract>,
        type_defs: HashMap<String, TypeDef>,
    ) -> Self {
        FfiRuntime {
            extern_funcs,
            ffi_contracts,
            type_defs,
            loaded_libs: Vec::new(),
            verify_ffi: true,
            signal_guard: true,
            runner: None,
        }
    }

    /// Collect capability definitions: cap_name -> list of component caps.
    pub(in crate::interp) fn collect_caps(item: &Item, out: &mut HashMap<String, Vec<String>>) {
        match item {
            Item::Cap(cap) => {
                let components = if let Some(ref combined) = cap.combined_with {
                    // Parse "A + B" format
                    let parts: Vec<String> = combined
                        .split(" + ")
                        .map(|s| s.trim().to_string())
                        .collect();
                    if parts.len() > 1 {
                        parts
                    } else {
                        vec![cap.name.clone(), combined.clone()]
                    }
                } else {
                    vec![cap.name.clone()]
                };
                out.insert(cap.name.clone(), components);
            }
            Item::Module(m) => {
                for inner in &m.items {
                    Self::collect_caps(inner, out);
                }
            }
            _ => {}
        }
    }

    fn collect_caps_all(file: &File) -> HashMap<String, Vec<String>> {
        let mut cap_defs = HashMap::new();
        for item in &file.items {
            Self::collect_caps(item, &mut cap_defs);
        }
        cap_defs
    }

    /// Collect type definitions: type_name -> TypeDef.
    pub(in crate::interp) fn collect_type_defs(item: &Item, out: &mut HashMap<String, TypeDef>) {
        match item {
            Item::Type(t) => {
                out.insert(t.name.clone(), t.clone());
            }
            Item::Actor(actor) => {
                let actor_type_def = TypeDef {
                    meta: AstNodeMeta::inherited(
                        actor.meta.span,
                        AstOrigin::RuntimeSystem("interp.actor_type"),
                    ),
                    name: actor.name.clone(),
                    pub_: actor.pub_,
                    kind: TypeDefKind::Record(
                        actor
                            .fields
                            .iter()
                            .map(|f| Field {
                                meta: f.meta,
                                name: f.name.clone(),
                                ty: f.ty.clone(),
                            })
                            .collect(),
                    ),
                    generics: Vec::new(),
                    derives: Vec::new(),
                    attributes: Vec::new(),
                };
                out.insert(actor.name.clone(), actor_type_def);
            }
            Item::Module(m) => {
                for inner in &m.items {
                    Self::collect_type_defs(inner, out);
                }
            }
            _ => {}
        }
    }

    /// Collect extern function declarations and their FFI contracts.
    pub(in crate::interp) fn collect_extern_funcs(
        item: &Item,
        out: &mut HashMap<String, ExternFunc>,
        contracts: &mut HashMap<String, FfiContract>,
        cap_defs: &HashMap<String, Vec<String>>,
        type_defs: &HashMap<String, TypeDef>,
    ) {
        let cap_names: std::collections::HashSet<String> = cap_defs.keys().cloned().collect();
        let record_type_names: std::collections::HashSet<String> = type_defs
            .iter()
            .filter(|(_, td)| matches!(td.kind, TypeDefKind::Record(_)))
            .map(|(name, _)| name.clone())
            .collect();
        let repr_c_record_names: std::collections::HashSet<String> = type_defs
            .iter()
            .filter(|(_, td)| td.attributes.contains(&TypeAttribute::ReprC))
            .map(|(name, _)| name.clone())
            .collect();
        match item {
            Item::ExternBlock(block) => {
                let no_panic = block.no_panic;
                for func in &block.funcs {
                    let mut f = func.clone();
                    // Propagate block-level no_panic to each function
                    if no_panic {
                        f.no_panic = true;
                    }
                    out.insert(f.name.clone(), f);
                    contracts.insert(
                        func.name.clone(),
                        FfiContract::from_extern_with_caps_repr(
                            func,
                            &cap_names,
                            &record_type_names,
                            &repr_c_record_names,
                        ),
                    );
                }
            }
            Item::Module(m) => {
                for inner in &m.items {
                    Self::collect_extern_funcs(inner, out, contracts, cap_defs, type_defs);
                }
            }
            _ => {}
        }
    }

    /// Execute an extern function call through its FFI contract, with the
    /// execution engine supplied by the caller.
    ///
    /// SAFETY: `runner_ptr` must point to a live engine for the duration of
    /// the synchronous C call (the caller's stack frame). The pointer is
    /// stored only for that duration and cleared before returning.
    pub(in crate::interp) fn call_extern_with_runner_ptr(
        &mut self,
        extern_func: &ExternFunc,
        contract: &FfiContract,
        args: Vec<Value>,
        runner_ptr: *mut (dyn FfiClosureRunner + 'static),
    ) -> Result<Value, Errno> {
        self.runner = Some(runner_ptr);
        let result = self.call_extern(extern_func, contract, args);
        self.runner = None;
        result
    }

    /// Execute an extern function call through its FFI contract.
    ///
    /// `self.runner` (set via `set_runner` / `call_extern_with_runner_ptr`)
    /// is the execution engine for any callback parameters (closures passed
    /// to C). The engine must stay alive for the duration of the synchronous
    /// C call.
    pub(in crate::interp) fn call_extern(
        &mut self,
        extern_func: &ExternFunc,
        contract: &FfiContract,
        args: Vec<Value>,
    ) -> Result<Value, Errno> {
        debug_assert!(
            self.runner.is_some(),
            "FfiRuntime::call_extern requires set_runner() first"
        );
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
        let mut string_guards: Vec<CString> = Vec::new();
        let mut shared_handles: Vec<Arc<crate::ffi::runtime::SharedHandle>> = Vec::new();
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
        let lib_idx = if let Some(idx) = self
            .loaded_libs
            .iter()
            .position(|(path, _)| path == &lib_path)
        {
            idx
        } else {
            // SAFETY: libloading::Library::new loads a shared library via FFI; the path is guaranteed valid by environment variable check above.
            unsafe {
                let lib = libloading::Library::new(&lib_path).map_err(|e| {
                    Errno::Generic(format!("failed to load library '{}': {}", lib_path, e))
                })?;
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
                // SAFETY: __errno_location returns a valid thread-local pointer
                // to the current errno variable on Linux/Android.
                unsafe {
                    *libc::__errno_location() = 0;
                }
                #[cfg(target_os = "macos")]
                // SAFETY: __error returns a valid thread-local pointer to the
                // current errno variable on macOS.
                unsafe {
                    *libc::__error() = 0;
                }
            }

            // Build libffi type descriptors for arguments
            let mut cif_arg_types: Vec<FfiType> = Vec::with_capacity(contract.args.len());
            for arg_contract in &contract.args {
                match arg_contract {
                    FfiArgContract::Float => cif_arg_types.push(FfiType::f64()),
                    FfiArgContract::Callback { .. } => cif_arg_types.push(FfiType::pointer()),
                    FfiArgContract::StructByValue(type_name) => {
                        let fields = self
                            .lookup_struct_fields(type_name)
                            .map_err(Errno::Generic)?;
                        let field_types: Result<Vec<FfiType>, String> = fields
                            .iter()
                            .map(|f| self.ffi_type_from_mimi_type(&f.ty))
                            .collect();
                        let field_types = field_types.map_err(Errno::Generic)?;
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
                FfiRetContract::String | FfiRetContract::StringOwned | FfiRetContract::Json => {
                    FfiType::pointer()
                }
                FfiRetContract::StructByValue(type_name) => {
                    let fields = self
                        .lookup_struct_fields(type_name)
                        .map_err(Errno::Generic)?;
                    let (total_size, _) =
                        self.struct_size_align(&fields).map_err(Errno::Generic)?;
                    struct_ret_size = Some(total_size);
                    let field_types: Result<Vec<FfiType>, String> = fields
                        .iter()
                        .map(|f| self.ffi_type_from_mimi_type(&f.ty))
                        .collect();
                    let field_types = field_types.map_err(Errno::Generic)?;
                    FfiType::structure(field_types)
                }
                _ => FfiType::i64(),
            };

            let cif = Cif::new(cif_arg_types, cif_ret_type);

            // Prepare typed arguments for libffi call
            let mut typed_storage: Vec<Box<dyn std::any::Any>> =
                Vec::with_capacity(contract.args.len());
            let mut ffi_args: Vec<libffi::middle::Arg> = Vec::with_capacity(contract.args.len());

            for (i, (arg_val, arg_contract)) in args.iter().zip(&contract.args).enumerate() {
                match arg_contract {
                    FfiArgContract::Float => {
                        let f = match arg_val {
                            Value::Float(f) => *f,
                            Value::Int(n) => *n as f64,
                            _ => {
                                return Err(Errno::Generic(
                                    "FFI contract violation: expected float or int".to_string(),
                                ))
                            }
                        };
                        typed_storage.push(Box::new(f));
                        let last = typed_storage.last().ok_or_else(|| {
                            "FFI call: typed_storage is empty after push (impossible)".to_string()
                        })?;
                        let ptr = last.downcast_ref::<f64>().ok_or_else(|| {
                            "FFI call: expected f64 in typed_storage but downcast failed"
                                .to_string()
                        })?;
                        ffi_args.push(ffi_arg(ptr));
                    }
                    FfiArgContract::StructByValue(type_name) => {
                        let fields = self
                            .lookup_struct_fields(type_name)
                            .map_err(Errno::Generic)?;
                        let buffer = self
                            .marshall_record_to_buffer(arg_val, &fields)
                            .map_err(Errno::Generic)?;
                        // SAFETY: Buffer heap data is stable (Vec only moves handle).
                        // Arg::new stores a raw data pointer; struct_buffers keeps
                        // the buffer alive for the synchronous C call.
                        let data_ptr = buffer.as_ptr() as *mut std::ffi::c_void;
                        // Create Arg pointing to the first byte of buffer data.
                        // SAFETY: data_ptr points to the first byte of a live buffer
                        // stored in struct_buffers, which outlives the C call.
                        // ffi_arg only borrows the address for argument setup.
                        let arg = unsafe { ffi_arg(&*data_ptr) };
                        struct_buffers.push(buffer);
                        ffi_args.push(arg);
                        typed_storage.push(Box::new(0i64)); // placeholder
                    }
                    _ => {
                        let v = c_args[i];
                        typed_storage.push(Box::new(v));
                        let last = typed_storage.last().ok_or_else(|| {
                            "FFI call: typed_storage is empty after push (impossible)".to_string()
                        })?;
                        let ptr = last.downcast_ref::<i64>().ok_or_else(|| {
                            "FFI call: expected i64 in typed_storage but downcast failed"
                                .to_string()
                        })?;
                        ffi_args.push(ffi_arg(ptr));
                    }
                }
            }

            let lib = &self.loaded_libs[lib_idx].1;
            // Get the function pointer as a raw address for libffi
            // SAFETY: lib is a live Library, and func_name is a valid symbol name
            // in that library. The returned Symbol borrows from lib, which remains
            // alive for the duration of this block.
            let raw_fn: libloading::Symbol<*mut std::ffi::c_void> = unsafe {
                lib.get(func_name.as_bytes())
                    .map_err(|e| format!("failed to find symbol '{}': {}", func_name, e))?
            };
            let fn_ptr = *raw_fn;
            let code_ptr = CodePtr(fn_ptr);

            // F8: Set up thread-local callback context if any callback contracts exist
            let has_callbacks = contract
                .args
                .iter()
                .any(|a| matches!(a, FfiArgContract::Callback { .. }));
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
                        reentrancy_depth: ctx.reentrancy_depth,
                    }
                }));
                // SAFETY: self.runner was set by the caller (set_runner) and lives for
                // the duration of the synchronous C call. The C call may invoke
                // callbacks on the same thread, which will read this context.
                let runner_ptr: *mut dyn FfiClosureRunner = match self.runner {
                    Some(p) => p,
                    None => {
                        return Err(Errno::Generic(
                            "FfiRuntime: no execution engine (runner) set for extern call".into(),
                        ))
                    }
                };
                // SAFETY: The runner outlives the synchronous C call.
                // CRITICAL #4 analysis: The lifetime erasure to 'static is sound
                // ONLY because:
                //   1. The C call is synchronous — it runs to completion before
                //      this function returns, so the runner is still alive.
                //   2. Callbacks execute on the same thread during the C call.
                //   3. The pointer is NOT stored beyond the C call's scope.
                //   4. If C stores the callback pointer for later async use,
                //      this would be use-after-free — but our FFI contract
                //      requires callbacks to be invoked synchronously only.
                // The previous context is restored after the call (see prev_ctx
                // restoration below), ensuring no stale pointers survive.
                super::ffi::callback::FFI_CALLBACK_CTX.with(|c| {
                    let mut ctx = c.borrow_mut();
                    ctx.interp = Some(runner_ptr);
                });
            }

            // Call via libffi with correct ABI and crash protection.
            // SD-4: signal guard replaces fork isolation (POSIX UB in multi-threaded).
            // Struct-by-value return uses custom rvalue buffer; other paths use scalar return.
            let call_result: Result<i64, String> = if let Some(buf_size) = struct_ret_size {
                // Allocate zeroed buffer for the struct return value.
                let mut ret_buf = vec![0u8; buf_size];
                let rvalue = ret_buf.as_mut_ptr() as *mut std::ffi::c_void;
                // SD-4: signal guard for struct-by-value returns.
                if self.signal_guard || extern_func.no_panic {
                    super::ffi::signal_guard::call_guarded(|| {
                        // SAFETY: call_ffi_raw_struct uses the low-level ffi_call API
                        // with a caller-provided return buffer. rvalue points to a valid
                        // ret_buf allocation.
                        unsafe {
                            Self::call_ffi_raw_struct(&cif, code_ptr, &ffi_args, rvalue);
                        }
                    })?;
                } else {
                    // SAFETY: call_ffi_raw_struct uses the low-level ffi_call API
                    // with a caller-provided return buffer. rvalue points to a valid
                    // ret_buf allocation.
                    unsafe {
                        Self::call_ffi_raw_struct(&cif, code_ptr, &ffi_args, rvalue);
                    }
                }
                struct_buffers.push(ret_buf);
                Ok(0i64) // placeholder; actual result read from buffer below
            } else if self.signal_guard || extern_func.no_panic {
                // SD-4: signal guard for scalar returns.
                super::ffi::signal_guard::call_guarded(|| {
                    // SAFETY: call_ffi_raw is an unsafe fn; its contract is satisfied
                    // by the valid CIF, code pointer, and argument slice.
                    unsafe { Self::call_ffi_raw(&cif, code_ptr, &ffi_args, &contract.ret) }
                })
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
                        ctx.interp = None;
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
                let fields = self
                    .lookup_struct_fields(type_name)
                    .map_err(Errno::Generic)?;
                self.unmarshall_buffer_to_record(&ret_buf, &fields)?
            } else {
                return Err(Errno::Generic(
                    "FFI wrapper: struct return buffer missing".to_string(),
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

    // ── contract verification (engine-agnostic) ─────────────────────────

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

    /// Stage 4: Check precondition (requires) before the C call.
    /// Evaluated through the active engine (`self.runner`).
    fn verify_ffi_requires(
        &mut self,
        extern_func: &ExternFunc,
        contract: &FfiContract,
    ) -> Result<(), Errno> {
        if let Some(requires_expr) = &contract.requires {
            // SAFETY: self.runner is Some (set_runner called by the caller)
            // and valid for the duration of the synchronous call.
            let runner = match self.runner {
                Some(p) => p,
                None => {
                    return Err(Errno::Generic(
                        "FfiRuntime: no execution engine (runner) set for extern call".into(),
                    ))
                }
            };
            let runner = unsafe { &mut *runner };
            let result = runner.eval_contract_expr(requires_expr, None);
            match result {
                Ok(val) if super::value::is_truthy(&val) => { /* precondition holds */ }
                Ok(_) => {
                    return Err(Errno::Generic(format!(
                        "FFI contract violation: precondition of '{}' failed",
                        extern_func.name
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
        Ok(())
    }

    /// Stage 4: Check postcondition (ensures) after the C call.
    /// Binds 'result' to the return value for ensures evaluation.
    fn verify_ffi_ensures(
        &mut self,
        extern_func: &ExternFunc,
        contract: &FfiContract,
        return_value: &Value,
    ) -> Result<(), Errno> {
        if let Some(ensures_expr) = &contract.ensures {
            // SAFETY: self.runner is Some (set_runner called by the caller)
            // and valid for the duration of the synchronous call.
            let runner = match self.runner {
                Some(p) => p,
                None => {
                    return Err(Errno::Generic(
                        "FfiRuntime: no execution engine (runner) set for extern call".into(),
                    ))
                }
            };
            let runner = unsafe { &mut *runner };
            let result = runner.eval_contract_expr(ensures_expr, Some(return_value));
            match result {
                Ok(val) if super::value::is_truthy(&val) => { /* postcondition holds */ }
                Ok(_) => {
                    return Err(Errno::Generic(format!(
                        "FFI contract violation: postcondition of '{}' failed",
                        extern_func.name
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
        Ok(())
    }

    // ── struct-by-value helpers ──────────────────────────────────────────

    /// Look up the record fields for a StructByValue type name.
    fn lookup_struct_fields(&self, type_name: &str) -> Result<Vec<Field>, String> {
        let td = self
            .type_defs
            .get(type_name)
            .ok_or_else(|| format!("StructByValue: type '{}' not found in type_defs", type_name))?;
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
    fn ffi_type_from_mimi_type(&self, ty: &Type) -> Result<FfiType, String> {
        match ty.unlocated() {
            Type::Name(name, _) => match name.as_str() {
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
    fn mimi_type_size_align(&self, ty: &Type) -> Result<(usize, usize), String> {
        match ty.unlocated() {
            Type::Name(name, _) => match name.as_str() {
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
    fn marshall_record_to_buffer(&self, val: &Value, fields: &[Field]) -> Result<Vec<u8>, String> {
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
                format!(
                    "StructByValue: field '{}' missing in record value",
                    field.name
                )
            })?;
            self.write_field_to_buf(fv, &field.ty, &mut buf, offset)?;
        }

        Ok(buf)
    }

    /// Write a single Mimi value into a byte buffer at the given offset,
    /// using the C ABI scalar layout (little-endian).
    fn write_field_to_buf(
        &self,
        val: &Value,
        ty: &Type,
        buf: &mut [u8],
        offset: usize,
    ) -> Result<(), String> {
        let type_name = match ty.unlocated() {
            Type::Name(n, _) => n.as_str(),
            _ => {
                return Err(format!(
                    "StructByValue: cannot write field of type {:?}",
                    ty
                ))
            }
        };
        match type_name {
            "i32" => {
                let v = match val {
                    Value::Int(n) => *n as i32,
                    Value::Bool(b) => *b as i32,
                    _ => return Err(format!("StructByValue: expected i32, got {}", val)),
                };
                let bytes = v.to_le_bytes();
                buf[offset..offset + 4].copy_from_slice(&bytes);
            }
            "i64" => {
                let v = match val {
                    Value::Int(n) => *n,
                    _ => return Err(format!("StructByValue: expected i64, got {}", val)),
                };
                let bytes = v.to_le_bytes();
                buf[offset..offset + 8].copy_from_slice(&bytes);
            }
            "f64" => {
                let v = match val {
                    Value::Float(f) => *f,
                    Value::Int(n) => *n as f64,
                    _ => return Err(format!("StructByValue: expected f64, got {}", val)),
                };
                let bytes = v.to_bits().to_le_bytes();
                buf[offset..offset + 8].copy_from_slice(&bytes);
            }
            "bool" => {
                let v = match val {
                    Value::Bool(b) => *b as u8,
                    Value::Int(n) => {
                        if *n == 0 {
                            0u8
                        } else {
                            1u8
                        }
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
                                    buf[offset..offset + len].copy_from_slice(&sub_buf);
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
    fn unmarshall_buffer_to_record(&self, buf: &[u8], fields: &[Field]) -> Result<Value, Errno> {
        let mut field_vals = HashMap::new();
        let mut current_offset = 0usize;
        for field in fields {
            let (size, align) = self
                .mimi_type_size_align(&field.ty)
                .map_err(Errno::Generic)?;
            let aligned = (current_offset + align - 1) & !(align - 1);
            let val = self.read_field_from_buf(buf, &field.ty, aligned)?;
            field_vals.insert(field.name.clone(), val);
            current_offset = aligned + size;
        }
        Ok(Value::Record(None, field_vals))
    }

    /// Read a single field value from a byte buffer at the given offset.
    fn read_field_from_buf(&self, buf: &[u8], ty: &Type, offset: usize) -> Result<Value, Errno> {
        let type_name = match ty.unlocated() {
            Type::Name(n, _) => n.as_str(),
            _ => {
                return Err(Errno::Generic(format!(
                    "StructByValue: cannot read field of type {:?}",
                    ty
                )))
            }
        };
        match type_name {
            "i32" => {
                let mut bytes = [0u8; 4];
                bytes.copy_from_slice(&buf[offset..offset + 4]);
                Ok(Value::Int(i32::from_le_bytes(bytes) as i64))
            }
            "i64" => {
                let mut bytes = [0u8; 8];
                bytes.copy_from_slice(&buf[offset..offset + 8]);
                Ok(Value::Int(i64::from_le_bytes(bytes)))
            }
            "f64" => {
                let mut bytes = [0u8; 8];
                bytes.copy_from_slice(&buf[offset..offset + 8]);
                Ok(Value::Float(f64::from_le_bytes(bytes)))
            }
            "bool" => Ok(Value::Bool(buf[offset] != 0)),
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
                    "StructByValue: unsupported field type '{}' in record return",
                    other
                )))
            }
        }
    }

    // ── C ABI call helpers ───────────────────────────────────────────────

    /// Call a C function via libffi (raw, standalone — no self access).
    ///
    /// SAFETY: `cif` and `code_ptr` must describe a valid C function and ABI.
    /// `ffi_args` must be valid libffi arguments whose lifetimes exceed the call.
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

    /// Call a C function that returns a struct by value, writing into a
    /// caller-provided buffer. Uses the low-level `raw::ffi_call` API to
    /// supply a custom return-value buffer of the struct's size.
    unsafe fn call_ffi_raw_struct(
        cif: &Cif,
        code_ptr: CodePtr,
        ffi_args: &[libffi::middle::Arg],
        rvalue: *mut std::ffi::c_void,
    ) {
        // SAFETY: rvalue must be a valid, writable buffer of sufficient
        // size for the struct return type. cif.as_raw_ptr() provides a
        // valid CIF descriptor for libffi.
        // IP-C6: reject null code pointers before calling into libffi.
        if code_ptr.as_ptr().is_null() {
            return;
        }
        let fn_ptr = unsafe { *code_ptr.as_safe_fun() };
        // SAFETY: ffi_call is called with a valid CIF, function pointer, return
        // buffer, and argument array; all lifetimes exceed this call.
        unsafe {
            libffi::raw::ffi_call(
                cif.as_raw_ptr(),
                Some(fn_ptr),
                rvalue,
                ffi_args.as_ptr() as *mut *mut std::ffi::c_void,
            );
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
        // SAFETY: call_ffi_raw is an unsafe fn; its contract is satisfied by the
        // valid CIF, code pointer, and argument slice passed by call_extern.
        unsafe { Ok(Self::call_ffi_raw(cif, code_ptr, ffi_args, ret_contract)) }
    }
}

impl FfiRuntime {
    /// Convert a single Mimi value into a C ABI argument according to the
    /// argument's FFI contract.
    #[allow(clippy::too_many_arguments)]
    fn value_to_ffi_arg(
        &self,
        arg: &Value,
        contract: &FfiArgContract,
        string_guards: &mut Vec<CString>,
        shared_handles: &mut Vec<Arc<crate::ffi::runtime::SharedHandle>>,
        ffi_guards: &mut Vec<FfiGuard>,
        shared_guard: &mut FfiSharedGuard,
        shared_dedup: &mut HashMap<*const (), i64>,
        callback_ids: &mut Vec<i64>,
    ) -> Result<i64, Errno> {
        match contract {
            FfiArgContract::Int(_) => match arg {
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
                    // IP-C3: allocate with libc::malloc so C's free(3) is always
                    // correct (Rust CString::into_raw may use a different heap
                    // on musl/Windows/custom allocators).
                    let sanitized: String = s.as_str().chars().filter(|&c| c != '\0').collect();
                    let bytes = sanitized.as_bytes();
                    let n = bytes.len() + 1;
                    // SAFETY: malloc(n) for n >= 1; copy NUL-terminated payload.
                    let ptr = unsafe { libc::malloc(n) as *mut i8 };
                    if ptr.is_null() {
                        return Err(Errno::Generic(
                            "FFI wrapper: malloc failed for StringTransfer".to_string(),
                        ));
                    }
                    unsafe {
                        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len());
                        *ptr.add(bytes.len()) = 0;
                    }
                    Ok(ptr as i64)
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
                        crate::ast::CapMode::Move => {
                            // Register as a consumed cap (move semantics)
                            let cap_id = cap_table_register(&cap_name);
                            cap_table_consume(cap_id, &cap_name);
                            Ok(cap_id)
                        }
                        crate::ast::CapMode::Borrow => {
                            // Register as a non-consumed cap (borrow semantics)
                            Ok(cap_table_register(&cap_name))
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
                // 0.33 Phase D: the bytecode VM compiles `&x`/`&mut x` to a
                // value-move (no-op), so the `Value::Ref` shape never reaches
                // this point. Reject borrowed-reference params at the
                // type-contract level so both backends agree.
                if ty.starts_with("Ref(") || ty.starts_with("RefMut(") {
                    Err(Errno::Generic(format!(
                        "FFI safety: cannot pass borrowed reference type '{}' directly to \
                         extern function. Use a passport type such as c_borrow T or \
                         c_borrow_mut T instead.",
                        ty
                    )))
                } else {
                    Err(self.unsupported_ffi_arg_error(arg, ty))
                }
            }
            FfiArgContract::Callback { param_types, ret_type } => {
                self.value_to_ffi_callback(
                    arg,
                    param_types,
                    ret_type,
                    string_guards,
                    shared_handles,
                    ffi_guards,
                    callback_ids,
                )
            }
            FfiArgContract::StructByValue(_) => {
                Err(Errno::Generic(
                    "FFI wrapper: StructByValue args are handled in the call_extern CIF/arg loop, not via value_to_ffi_arg".to_string()
                ))
            }
            FfiArgContract::RawPtr(_) => match arg {
                // *T: immutable raw pointer
                Value::Shared(arc) => {
                    let arc_ptr = Arc::as_ptr(arc) as *const ();
                    if let Some(&existing_id) = shared_dedup.get(&arc_ptr) {
                        if let Some(handle) = shared_table_get(existing_id) {
                            shared_handles.push(handle.clone());
                            let guard = arc.read().map_err(|e| Errno::Generic(format!("read lock failed: {}", e)))?;
                            let ptr = &*guard as *const Value as *const () as i64;
                            // SAFETY: guard created from arc.read(), same Arc stored in FfiGuard.
                            // Guard is dropped before Arc (struct field order), so data stays alive.
                            ffi_guards.push(ffi_guard_new_read(guard, Arc::clone(arc)));
                            Ok(ptr)
                        } else {
                            Err(Errno::Generic("FFI wrapper: shared handle missing from table during raw ptr dedup".to_string()))
                        }
                    } else {
                        let handle_id = shared_table_create_dedup(Arc::clone(arc), Arc::as_ptr(arc) as *const ());
                        shared_dedup.insert(arc_ptr, handle_id);
                        shared_guard.register(handle_id);
                        if let Some(handle) = shared_table_get(handle_id) {
                            shared_handles.push(handle.clone());
                            let guard = arc.read().map_err(|e| Errno::Generic(format!("read lock failed: {}", e)))?;
                            let ptr = &*guard as *const Value as *const () as i64;
                            // SAFETY: guard created from arc.read(), same Arc stored in FfiGuard.
                            // Guard is dropped before Arc (struct field order), so data stays alive.
                            ffi_guards.push(ffi_guard_new_read(guard, Arc::clone(arc)));
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
                    // SAFETY: `ffi_guard_new_read` pairs the guard with its Arc for correct drop order.
                    ffi_guards.push(ffi_guard_new_read(guard, Arc::clone(rc)));
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
                        if let Some(handle) = shared_table_get(existing_id) {
                            shared_handles.push(handle.clone());
                            let mut guard = arc.write().map_err(|e| Errno::Generic(format!("write lock failed: {}", e)))?;
                            let ptr = &mut *guard as *mut Value as *mut () as i64;
                            // SAFETY: guard created from arc.write(), same Arc stored in FfiGuard.
                            // Guard is dropped before Arc (struct field order), so data stays alive.
                            ffi_guards.push(ffi_guard_new_write(guard, Arc::clone(arc)));
                            Ok(ptr)
                        } else {
                            Err(Errno::Generic("FFI wrapper: shared handle missing from table during raw ptr mut dedup".to_string()))
                        }
                    } else {
                        let handle_id = shared_table_create_dedup(Arc::clone(arc), Arc::as_ptr(arc) as *const ());
                        shared_dedup.insert(arc_ptr, handle_id);
                        shared_guard.register(handle_id);
                        if let Some(handle) = shared_table_get(handle_id) {
                            shared_handles.push(handle.clone());
                            let mut guard = arc.write().map_err(|e| Errno::Generic(format!("write lock failed: {}", e)))?;
                            let ptr = &mut *guard as *mut Value as *mut () as i64;
                            // SAFETY: guard created from arc.write(), same Arc stored in FfiGuard.
                            // Guard is dropped before Arc (struct field order), so data stays alive.
                            ffi_guards.push(ffi_guard_new_write(guard, Arc::clone(arc)));
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
                    // SAFETY: `ffi_guard_new_write` pairs the guard with its Arc for correct drop order.
                    ffi_guards.push(ffi_guard_new_write(guard, Arc::clone(rc)));
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
                        let handle_id = shared_table_create_dedup(Arc::clone(arc), Arc::as_ptr(arc) as *const ());
                        shared_dedup.insert(arc_ptr, handle_id);
                        shared_guard.register(handle_id);
                        Ok(handle_id)
                    }
                }
                Value::LocalShared(rc) => {
                    // Clone the inner value into an Arc<RwLock> for SharedHandle.
                    // The original local_shared retains its local refcount; the FFI
                    // side gets an independent shared copy via the handle table.
                    let value = rc.0.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    let arc = Arc::new(RwLock::new(value));
                    let handle_id = shared_table_create(arc);
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
                        if let Some(handle) = shared_table_get(existing_id) {
                            shared_handles.push(handle.clone());
                            let guard = arc.read().map_err(|e| Errno::Generic(format!("read lock failed: {}", e)))?;
                            let ptr = &*guard as *const Value as *const () as i64;
                            // SAFETY: guard created from arc.read(), same Arc stored in FfiGuard.
                            // Guard is dropped before Arc (struct field order), so data stays alive.
                            ffi_guards.push(ffi_guard_new_read(guard, Arc::clone(arc)));
                            Ok(ptr)
                        } else {
                            Err(Errno::Generic("FFI wrapper: shared handle missing from table during c_borrow dedup".to_string()))
                        }
                    } else {
                        let handle_id = shared_table_create_dedup(Arc::clone(arc), Arc::as_ptr(arc) as *const ());
                        shared_dedup.insert(arc_ptr, handle_id);
                        shared_guard.register(handle_id);
                        if let Some(handle) = shared_table_get(handle_id) {
                            shared_handles.push(handle.clone());
                            let guard = arc.read().map_err(|e| Errno::Generic(format!("read lock failed: {}", e)))?;
                            let ptr = &*guard as *const Value as *const () as i64;
                            // SAFETY: guard created from arc.read(), same Arc stored in FfiGuard.
                            // Guard is dropped before Arc (struct field order), so data stays alive.
                            ffi_guards.push(ffi_guard_new_read(guard, Arc::clone(arc)));
                            Ok(ptr)
                        } else {
                            Err(Errno::Generic("FFI wrapper: failed to create shared handle for c_borrow".to_string()))
                        }
                    }
                }
                Value::Ref(rc) => {
                    let guard = rc.read().map_err(|e| Errno::Generic(format!("read lock failed: {}", e)))?;
                    let ptr = &*guard as *const Value as *const () as i64;
                    // SAFETY: `ffi_guard_new_read` pairs the guard with its Arc for correct drop order.
                    ffi_guards.push(ffi_guard_new_read(guard, Arc::clone(rc)));
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
                        if let Some(handle) = shared_table_get(existing_id) {
                            shared_handles.push(handle.clone());
                            let mut guard = arc.write().map_err(|e| Errno::Generic(format!("write lock failed: {}", e)))?;
                            let ptr = &mut *guard as *mut Value as *mut () as i64;
                            // SAFETY: guard created from arc.write(), same Arc stored in FfiGuard.
                            // Guard is dropped before Arc (struct field order), so data stays alive.
                            ffi_guards.push(ffi_guard_new_write(guard, Arc::clone(arc)));
                            Ok(ptr)
                        } else {
                            Err(Errno::Generic("FFI wrapper: shared handle missing from table during c_borrow_mut dedup".to_string()))
                        }
                    } else {
                        let handle_id = shared_table_create_dedup(Arc::clone(arc), Arc::as_ptr(arc) as *const ());
                        shared_dedup.insert(arc_ptr, handle_id);
                        shared_guard.register(handle_id);
                        if let Some(handle) = shared_table_get(handle_id) {
                            shared_handles.push(handle.clone());
                            let mut guard = arc.write().map_err(|e| Errno::Generic(format!("write lock failed: {}", e)))?;
                            let ptr = &mut *guard as *mut Value as *mut () as i64;
                            // SAFETY: guard created from arc.write(), same Arc stored in FfiGuard.
                            // Guard is dropped before Arc (struct field order), so data stays alive.
                            ffi_guards.push(ffi_guard_new_write(guard, Arc::clone(arc)));
                            Ok(ptr)
                        } else {
                            Err(Errno::Generic("FFI wrapper: failed to create shared handle for c_borrow_mut".to_string()))
                        }
                    }
                }
                Value::RefMut(rc) => {
                    let mut guard = rc.write().map_err(|e| Errno::Generic(format!("write lock failed: {}", e)))?;
                    let ptr = &mut *guard as *mut Value as *mut () as i64;
                    // SAFETY: `ffi_guard_new_write` pairs the guard with its Arc for correct drop order.
                    ffi_guards.push(ffi_guard_new_write(guard, Arc::clone(rc)));
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
    fn ffi_ret_to_value(&self, result: i64, contract: &FfiRetContract) -> Result<Value, Errno> {
        match contract {
            FfiRetContract::Unit => Ok(Value::Unit),
            FfiRetContract::Int(crate::ffi::contract::FfiScalarType::Bool) => {
                Ok(Value::Bool(result != 0))
            }
            FfiRetContract::Int(crate::ffi::contract::FfiScalarType::I32) => {
                Ok(Value::Int(result as i32 as i64))
            }
            FfiRetContract::Int(_) => Ok(Value::Int(result)),
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
                    // SAFETY: The StringOwned contract requires the C function to return
                    // a valid, null-terminated string that Mimi will free. catch_unwind
                    // only catches Rust panics, not SIGSEGV from an invalid pointer.
                    let c_str = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        unsafe { std::ffi::CStr::from_ptr(result as *const i8) }
                    })).map_err(|_| format!(
                        "FFI safety: C function returned invalid string pointer (address {:#x})", result
                    ))?;
                    let s = c_str.to_string_lossy().into_owned();
                    // SAFETY: result is a non-null pointer returned under the StringOwned
                    // contract; Mimi takes ownership and must free it with libc::free.
                    unsafe { libc::free(result as *mut libc::c_void); }
                    Ok(Value::String(s))
                }
            }
            FfiRetContract::Json => {
                if result == 0 {
                    Ok(Value::Unit)
                } else {
                    // SAFETY: The Json contract requires the C function to return a
                    // valid, null-terminated string that Mimi will free. catch_unwind
                    // only catches Rust panics, not SIGSEGV from an invalid pointer.
                    let c_str = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        unsafe { std::ffi::CStr::from_ptr(result as *const i8) }
                    })).map_err(|_| format!(
                        "FFI safety: C function returned invalid JSON string pointer (address {:#x})", result
                    ))?;
                    let json_str = c_str.to_string_lossy();
                    let json_val: serde_json::Value = serde_json::from_str(&json_str)
                        .map_err(|e| format!("FFI: failed to parse JSON return value: {}", e))?;
                    // SAFETY: result is a non-null pointer returned under the Json
                    // contract; Mimi takes ownership and must free it with libc::free.
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
            FfiRetContract::StructByValue(ty) => Err(Errno::Generic(format!(
                "FFI safety: struct-by-value return for '{}' is not yet supported in the interpreter; use JSON fallback",
                ty
            ))),
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
            Value::Record(_, _) | Value::Variant(_, _) | Value::List(_) | Value::Tuple(_) | Value::Set(_) => {
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

    /// Convert a Mimi value to a serde_json::Value (used for Json FFI contracts
    /// and the to_json builtin).
    pub(in crate::interp) fn value_to_json(&self, v: &Value) -> Result<serde_json::Value, Errno> {
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
            Value::Set(items) => {
                // Sort for dual-backend stable to_json.
                let mut ints: Vec<i64> = Vec::new();
                let mut strs: Vec<String> = Vec::new();
                let mut bools: Vec<bool> = Vec::new();
                let mut floats: Vec<f64> = Vec::new();
                let mut other: Vec<serde_json::Value> = Vec::new();
                for i in items {
                    match i {
                        Value::Int(n) => ints.push(*n),
                        Value::String(s) => strs.push(s.clone()),
                        Value::Bool(b) => bools.push(*b),
                        Value::Float(f) => floats.push(*f),
                        other_v => other.push(self.value_to_json(other_v)?),
                    }
                }
                ints.sort_unstable();
                strs.sort();
                bools.sort_unstable();
                floats.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let mut arr: Vec<serde_json::Value> = ints
                    .into_iter()
                    .map(|n| serde_json::Value::Number(n.into()))
                    .collect();
                for s in strs {
                    arr.push(serde_json::Value::String(s));
                }
                for b in bools {
                    arr.push(serde_json::Value::Bool(b));
                }
                for f in floats {
                    if let Some(n) = serde_json::Number::from_f64(f) {
                        arr.push(serde_json::Value::Number(n));
                    }
                }
                // Stable dual order for non-scalar Set elements (Option/Result/tuples).
                other.sort_by_key(|jv| jv.to_string());
                arr.extend(other);
                Ok(serde_json::Value::Array(arr))
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
                    let arr: Result<Vec<_>, _> =
                        payload.iter().map(|i| self.value_to_json(i)).collect();
                    let mut map = serde_json::Map::new();
                    map.insert(name.clone(), serde_json::Value::Array(arr?));
                    Ok(serde_json::Value::Object(map))
                }
            }
            _ => Ok(serde_json::Value::String(format!("{}", v))),
        }
    }

    /// Convert a serde_json::Value back to a Mimi Value.
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
                let fields: HashMap<String, Value> = map
                    .iter()
                    .map(|(k, v)| (k.clone(), self.json_to_value(v)))
                    .collect();
                Value::Record(None, fields)
            }
        }
    }

    /// F8: Convert a Mimi closure value to a C-compatible callback function pointer.
    /// Registers the closure with the global callback table and creates a
    /// dynamically generated trampoline via libffi.
    #[allow(clippy::too_many_arguments)]
    fn value_to_ffi_callback(
        &self,
        arg: &Value,
        param_types: &[Type],
        ret_type: &Type,
        _string_guards: &mut Vec<CString>,
        _shared_handles: &mut Vec<Arc<crate::ffi::runtime::SharedHandle>>,
        ffi_guards: &mut Vec<FfiGuard>,
        callback_ids: &mut Vec<i64>,
    ) -> Result<i64, Errno> {
        // Ensure the program File is stored for cross-thread callback evaluation.
        // SAFETY: self.runner is Some (set_runner called by the caller).
        let runner = match self.runner {
            Some(p) => p,
            None => {
                return Err(Errno::Generic(
                    "FfiRuntime: no execution engine (runner) set for extern call".into(),
                ))
            }
        };
        let runner = unsafe { &mut *runner };
        super::ffi::callback::ensure_callback_file(runner.ffi_file());
        // 0.33 Phase D: register the VM's BytecodeProgram for cross-thread
        // BytecodeClosure evaluation. Valid while this extern call is on the
        // stack (C libraries join worker threads before returning).
        if let Some(program) = runner.ffi_bytecode_program() {
            super::ffi::callback::set_callback_program(program);
        }
        match arg {
            Value::Closure { .. } | Value::BytecodeClosure { .. } => {
                let closure = arg.clone();
                let ret_is_float =
                    matches!(ret_type.unlocated(), Type::Name(name, _) if name == "f64");

                // Build CIF matching the callback signature
                let mut cif_arg_types: Vec<FfiType> = Vec::with_capacity(param_types.len());
                for pt in param_types {
                    match pt.unlocated() {
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
                let cb_id = crate::ffi::callback_table_register(Some(Box::new(
                    |_id: i64, _args: &[i64]| -> i64 { 0 },
                )));
                callback_ids.push(cb_id);

                // F3: Store the closure in BOTH the thread-local context (fast path
                // for synchronous callbacks) and the global store (fallback for async/
                // off-thread callbacks where TLS has been cleared).
                let arg_free_mask = super::ffi::helpers::compute_arg_free_mask(param_types);
                let arg_kinds = super::ffi::helpers::compute_arg_kinds(param_types);
                // FFI-10: Per-callback active-call counter for deregister race prevention.
                let active_count = Arc::new(AtomicUsize::new(0));
                super::ffi::callback::FFI_CALLBACK_CTX.with(|c| {
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
                    super::ffi::callback::mimi_callback_trampoline_fn as libffi::low::Callback<i64, i64>,
                    cb_ref_static,
                );

                let code_ptr_ref = ffi_closure.code_ptr();
                // SAFETY: code_ptr_ref points to the libffi-generated trampoline.
                let fn_ptr_val: unsafe extern "C" fn() = *code_ptr_ref;
                let fn_ptr = fn_ptr_val as usize as i64;

                // SAFETY: reclaim userdata Box into keepalive alongside Closure.
                let keepalive = super::ffi::callback::CallbackTrampolineKeepalive {
                    _closure: Box::new(ffi_closure),
                    _userdata: unsafe { Box::from_raw(userdata_ptr) },
                };

                if let Ok(mut store) = super::ffi::callback::global_callback_store().lock() {
                    store.insert(
                        cb_id,
                        super::ffi::callback::GlobalCallbackEntry {
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
