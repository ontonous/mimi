use super::helpers::elem_type_tag;
use crate::ast::{Field, Type, TypeDefKind};
use crate::codegen::types;
use crate::codegen::{
    call_try_basic_value, CallSiteValueExt, CallbackThunkEntry, CodeGenerator, VarEntry,
};
use crate::error::{CompileError, MimiResult};
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum, FunctionType, StructType};
use inkwell::values::{
    BasicMetadataValueEnum, BasicValue, BasicValueEnum, CallSiteValue, PointerValue,
};
use inkwell::ThreadLocalMode;
use std::collections::HashMap;

/// LLVM types computed for an extern wrapper and its underlying C declaration.
#[allow(dead_code)]
struct ExternFnSignature<'ctx> {
    list_struct_sty: StructType<'ctx>,
    list_struct_ty: BasicTypeEnum<'ctx>,
    wrapper_ret_ty: BasicTypeEnum<'ctx>,
    wrapper_fn_type: FunctionType<'ctx>,
    extern_fn_type: FunctionType<'ctx>,
    is_complex_reprc_ret: bool,
    c_struct_ty: Option<StructType<'ctx>>,
}

impl<'ctx> CodeGenerator<'ctx> {
    fn type_to_llvm_for_extern(&self, ty: &crate::ast::Type) -> MimiResult<BasicTypeEnum<'ctx>> {
        // For user-defined types (Type::Name), prefer the registered type_llvm entry
        // which has the correct layout (e.g. i32 for #[repr(C)] enums).
        if let crate::ast::Type::Name(name, _) = ty {
            if let Some(&registered) = self.type_llvm.get(name.as_str()) {
                return Ok(registered);
            }
        }
        // G1b: Closure types (Type::Func) cross FFI as raw function pointers (i8*),
        // not as {fn_ptr, env_ptr} structs. The conversion is done at the call site
        // via get_or_create_callback_thunk + TLS globals.
        if matches!(ty, crate::ast::Type::Func(_, _)) {
            let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
            return Ok(BasicTypeEnum::PointerType(i8_ptr));
        }
        // Explicitly map unit to i64 (zero-size type has no C ABI representation of its own,
        // but extern wrappers always produce/consume an i64 to keep the function signature uniform).
        // This is NOT a fallback — it is the intended representation for unit in FFI.
        if let crate::ast::Type::Name(name, _) = ty {
            if name == "unit" {
                return Ok(BasicTypeEnum::IntType(self.context.i64_type()));
            }
        }
        types::mimi_type_to_llvm_extern(self.context, ty).ok_or_else(|| {
            CompileError::LlvmError(format!(
                "cannot map type '{}' to LLVM for extern FFI: type has no C ABI representation",
                crate::core::fmt_type(ty)
            ))
        })
    }

    pub(in crate::codegen) fn compile_impl_methods(&mut self) -> MimiResult<()> {
        for (type_name, trait_impls) in self.type_impls.clone() {
            for (trait_name, methods) in &trait_impls {
                for method in methods {
                    // Mangle name: {type_name}__{trait_name}__{method_name}
                    let mangled = format!("{}__{}__{}", type_name, trait_name, method.name);
                    // Build function: prepend self: &type_name as first param
                    let mut impl_method = method.clone();
                    impl_method.name = mangled;
                    // Include generic type args from the impl block so monomorphization
                    // can resolve e.g. List<T> → List<Item> when T=Item inside for loops.
                    let type_args_for_self = self
                        .impl_type_args
                        .get(&type_name)
                        .cloned()
                        .unwrap_or_default();
                    let self_type_name = if type_args_for_self.is_empty() {
                        Type::Name(type_name.clone(), vec![])
                    } else {
                        Type::Name(type_name.clone(), type_args_for_self.clone())
                    };
                    // Prepend self param: self: &type_name (or self: type_name for value types)
                    let self_ty = match type_name.as_str() {
                        // Scalar/value types: pass self by value.
                        "i32" | "i64" | "f64" | "bool" | "Record" | "Map" | "Any" => self_type_name,
                        _ => {
                            // Compound types: pass self by reference
                            crate::ast::Type::Ref(None, Box::new(self_type_name))
                        }
                    };
                    impl_method.params.insert(
                        0,
                        crate::ast::Param {
                            name: "self".into(),
                            ty: self_ty,
                            mut_: false,
                            default_value: None,
                borrow: None,
            },
                    );
                    // Set type_map to identity so compile_func can resolve type params
                    // in self (e.g., &List<T> → T resolves to T in type_map).
                    // This is later overridden by compile_generic_func for monomorphization.
                    let saved_type_map = self.type_map.clone();
                    if !type_args_for_self.is_empty() {
                        let mut identity_map: HashMap<String, Type> = HashMap::new();
                        for ta in &type_args_for_self {
                            if let Type::Name(tn, _) = ta {
                                identity_map.insert(tn.clone(), ta.clone());
                            }
                        }
                        // Only set when identity_map has entries and current type_map is empty
                        if !identity_map.is_empty() && self.type_map.is_empty() {
                            self.type_map.extend(identity_map);
                        }
                    }
                    self.compile_func(&impl_method)?;
                    self.type_map = saved_type_map;
                }
            }
        }
        Ok(())
    }

    /// Build vtable struct types and global vtable instances for all trait impls.
    /// Called after compile_impl_methods so mangled functions exist.
    pub(in crate::codegen) fn compile_vtables(&mut self) -> MimiResult<()> {
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        // Phase 1: define vtable struct type per trait
        let mut trait_method_list: HashMap<String, Vec<String>> = HashMap::new();
        for (trait_name, trait_def) in &self.trait_defs {
            let method_names: Vec<String> =
                trait_def.methods.iter().map(|m| m.name.clone()).collect();
            if method_names.is_empty() {
                continue;
            }
            // Vtable struct: one i8* (function pointer) per method
            let field_tys: Vec<BasicTypeEnum> = (0..method_names.len())
                .map(|_| BasicTypeEnum::PointerType(i8_ptr))
                .collect();
            let vtable_ty = self.context.struct_type(&field_tys, false);
            self.vtable_types.insert(trait_name.clone(), vtable_ty);
            trait_method_list.insert(trait_name.clone(), method_names);
        }

        // Phase 2: emit a global vtable constant for each (type, trait) impl pair
        for (type_name, trait_impls) in &self.type_impls {
            for (trait_name, methods) in trait_impls {
                let Some(vtable_ty) = self.vtable_types.get(trait_name) else {
                    continue;
                };
                let Some(expected_methods) = trait_method_list.get(trait_name) else {
                    continue;
                };

                // Build initializer: one bitcast(function) per method slot
                let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
                let mut fn_ptrs: Vec<BasicValueEnum> = Vec::new();
                for method_name in expected_methods {
                    if methods.iter().any(|m| &m.name == method_name) {
                        let mangled = format!("{}__{}__{}", type_name, trait_name, method_name);
                        if let Some(f) = self.module.get_function(&mangled) {
                            let ptr = self
                                .builder
                                .build_bit_cast(
                                    f.as_global_value().as_pointer_value(),
                                    i8_ptr,
                                    &format!("{}_{}_cast", trait_name, method_name),
                                )
                                .map_err(|e| {
                                    CompileError::LlvmError(format!("bitcast error: {}", e))
                                })?;
                            fn_ptrs.push(ptr);
                            continue;
                        }
                    }
                    fn_ptrs.push(i8_ptr.const_null().into());
                }
                if fn_ptrs.is_empty() {
                    continue;
                }
                let init_val = vtable_ty.const_named_struct(&fn_ptrs);
                let gv_name = format!("{}_{}_vtable", type_name, trait_name);
                let gv = self.module.add_global(*vtable_ty, None, &gv_name);
                gv.set_initializer(&init_val);
                gv.set_constant(true);
                self.vtable_globals
                    .insert(format!("{}__{}", type_name, trait_name), gv);
            }
        }
        Ok(())
    }

    /// G1b: Get or create a callback thunk for a given callback signature.
    /// The thunk is a small LLVM function that:
    ///     1. Reads fn_ptr and env_ptr from module-level globals
    ///     2. Calls fn_ptr(env_ptr, args...) with the correct C calling convention
    /// Returns the thunk function value and its two global slots.
    pub(in crate::codegen) fn get_or_create_callback_thunk(
        &mut self,
        cb_params: &[crate::ast::Type],
        cb_ret: &crate::ast::Type,
    ) -> MimiResult<CallbackThunkEntry<'ctx>> {
        // Build fingerprint from signature
        let ret_str = format!("{:?}", cb_ret);
        let params_str: Vec<String> = cb_params.iter().map(|t| format!("{:?}", t)).collect();
        let fingerprint = format!("{}_{}", ret_str, params_str.join("_"));

        // Check cache
        if let Some(entry) = self.callback_thunks.get(&fingerprint) {
            return Ok(CallbackThunkEntry {
                thunk_fn: entry.thunk_fn,
                fn_ptr_global: entry.fn_ptr_global,
                env_ptr_global: entry.env_ptr_global,
            });
        }

        let _i8_type = self.context.i8_type();
        let i8_ptr = self.context.ptr_type(inkwell::AddressSpace::default());
        let id = self.callback_thunk_counter;
        self.callback_thunk_counter += 1;

        // Create global slots for fn_ptr and env_ptr
        // F1: Use thread-local storage so concurrent parasteps threads each
        // see their own fn_ptr/env_ptr, preventing data races on the globals.
        let fn_ptr_global =
            self.module
                .add_global(i8_ptr, None, &format!("__mimi_cb_fnptr_{}", id));
        fn_ptr_global.set_initializer(&i8_ptr.const_null());
        fn_ptr_global.set_thread_local(true);
        fn_ptr_global.set_thread_local_mode(Some(ThreadLocalMode::GeneralDynamicTLSModel));
        let env_ptr_global =
            self.module
                .add_global(i8_ptr, None, &format!("__mimi_cb_envptr_{}", id));
        env_ptr_global.set_initializer(&i8_ptr.const_null());
        env_ptr_global.set_thread_local(true);
        env_ptr_global.set_thread_local_mode(Some(ThreadLocalMode::GeneralDynamicTLSModel));

        // Build thunk function type matching callback signature
        let mut thunk_param_tys = Vec::new();
        for pt in cb_params {
            let llvm_ty = types::mimi_type_to_llvm_extern(self.context, pt)
                .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));
            thunk_param_tys.push(types::basic_to_metadata(self.context, llvm_ty));
        }
        let thunk_ret_llvm = types::mimi_type_to_llvm_extern(self.context, cb_ret)
            .unwrap_or(BasicTypeEnum::IntType(self.context.i64_type()));

        let thunk_fn_type = match thunk_ret_llvm {
            BasicTypeEnum::IntType(t) => t.fn_type(&thunk_param_tys, false),
            BasicTypeEnum::FloatType(t) => t.fn_type(&thunk_param_tys, false),
            BasicTypeEnum::PointerType(t) => t.fn_type(&thunk_param_tys, false),
            BasicTypeEnum::StructType(t) => t.fn_type(&thunk_param_tys, false),
            BasicTypeEnum::ArrayType(t) => t.fn_type(&thunk_param_tys, false),
            _ => self.context.i64_type().fn_type(&thunk_param_tys, false),
        };

        let thunk_name = format!("__mimi_thunk_{}", id);
        let thunk_fn = self.module.add_function(
            &thunk_name,
            thunk_fn_type,
            Some(inkwell::module::Linkage::Internal),
        );

        let saved_block = self.builder.get_insert_block();
        let entry_bb = self.context.append_basic_block(thunk_fn, "entry");
        self.builder.position_at_end(entry_bb);

        // Load fn_ptr and env_ptr from global slots
        let fn_ptr_val = self.build_load(i8_ptr, fn_ptr_global.as_pointer_value(), "tls_fn_ptr")?;
        let env_ptr_val =
            self.build_load(i8_ptr, env_ptr_global.as_pointer_value(), "tls_env_ptr")?;
        let fn_ptr_pv = fn_ptr_val.into_pointer_value();
        let env_ptr_pv = env_ptr_val.into_pointer_value();

        // Build the Mimi closure function signature: fn(env_ptr: i8*, params...) -> ret
        let mut mimi_param_meta = vec![BasicMetadataTypeEnum::PointerType(i8_ptr)];
        mimi_param_meta.extend(thunk_param_tys.iter().cloned());

        let mimi_fn_ty = match thunk_ret_llvm {
            BasicTypeEnum::IntType(t) => t.fn_type(&mimi_param_meta, false),
            BasicTypeEnum::FloatType(t) => t.fn_type(&mimi_param_meta, false),
            BasicTypeEnum::PointerType(t) => t.fn_type(&mimi_param_meta, false),
            BasicTypeEnum::StructType(t) => t.fn_type(&mimi_param_meta, false),
            BasicTypeEnum::ArrayType(t) => t.fn_type(&mimi_param_meta, false),
            _ => self.context.i64_type().fn_type(&mimi_param_meta, false),
        };

        // Cast fn_ptr to the correct type: fn(env_ptr, params...) -> ret
        let fn_ptr_typed = self
            .builder
            .build_pointer_cast(
                fn_ptr_pv,
                self.context.ptr_type(inkwell::AddressSpace::default()),
                "fn_typed",
            )
            .map_err(|e| CompileError::LlvmError(format!("pointer cast: {}", e)))?;

        // Build call args: env_ptr + thunk params
        let mut call_args: Vec<BasicMetadataValueEnum<'ctx>> =
            vec![BasicMetadataValueEnum::PointerValue(env_ptr_pv)];
        for i in 0..thunk_param_tys.len() {
            let param = thunk_fn
                .get_nth_param(i as u32)
                .ok_or_else(|| CompileError::LlvmError(format!("thunk param {} not found", i)))?;
            call_args.push(match param {
                BasicValueEnum::IntValue(iv) => BasicMetadataValueEnum::IntValue(iv),
                BasicValueEnum::FloatValue(fv) => BasicMetadataValueEnum::FloatValue(fv),
                BasicValueEnum::PointerValue(pv) => BasicMetadataValueEnum::PointerValue(pv),
                BasicValueEnum::StructValue(sv) => BasicMetadataValueEnum::StructValue(sv),
                BasicValueEnum::ArrayValue(av) => BasicMetadataValueEnum::ArrayValue(av),
                _ => {
                    return Err(CompileError::LlvmError(format!(
                        "unsupported thunk param type at {}",
                        i
                    )))
                }
            });
        }

        let cb_call = self
            .builder
            .build_indirect_call(mimi_fn_ty, fn_ptr_typed, &call_args, "cb_call")
            .map_err(|e| CompileError::LlvmError(format!("thunk callback call: {}", e)))?;
        if thunk_fn_type.get_return_type().is_some() {
            let ret_val = call_try_basic_value(&cb_call).ok_or_else(|| {
                CompileError::LlvmError("thunk call returned void but expected value".to_string())
            })?;
            self.build_return(Some(&ret_val))?;
        } else {
            self.build_return(None)?;
        }

        if let Some(bb) = saved_block {
            self.builder.position_at_end(bb);
        }

        let entry = CallbackThunkEntry {
            thunk_fn,
            fn_ptr_global,
            env_ptr_global,
        };
        self.callback_thunks.insert(fingerprint, entry);

        Ok(CallbackThunkEntry {
            thunk_fn,
            fn_ptr_global,
            env_ptr_global,
        })
    }

    pub(in crate::codegen) fn register_extern_block(
        &mut self,
        block: &crate::ast::ExternBlock,
    ) -> MimiResult<()> {
        for ef in &block.funcs {
            // Store param types for later closure→thunk conversion in compile_call
            let param_types: Vec<crate::ast::Type> =
                ef.params.iter().map(|p| p.ty.clone()).collect();
            self.extern_param_types.insert(ef.name.clone(), param_types);

            // F7: Tuple is allowed via JSON serialization (same path as List/Record).
            // The interpreter path uses FfiArgContract::Json; the codegen path
            // uses mimi_tuple_serialize/mimi_tuple_deserialize runtime functions.
            //
            // ⚠️ F-4: JSON serialization inconsistency between interpreter and codegen.
            // Interpreter uses serde_json (nested key-value maps, full type support).
            // Codegen uses C runtime mimi_json_serialize (flat {i64*,len,type_tag},
            // limited to one level of nesting). Complex/nested record types may be
            // truncated or incorrectly serialized in the codegen path. This primarily
            // affects non-#[repr(C)] records passed as JSON across the FFI boundary.
            // #[repr(C)] records use struct-by-value and are not affected.

            // D: Reject Unsupported FFI contract types at codegen time with a
            // readable error rather than silently passing void* to C (UB risk).
            let contract = crate::ffi::FfiContract::from_extern_with_caps_repr(
                ef,
                &self.cap_type_names,
                &self.record_type_names,
                &self.repr_c_record_names,
            );
            for (i, arg_contract) in contract.args.iter().enumerate() {
                if let crate::ffi::contract::FfiArgContract::Unsupported(ty) = arg_contract {
                    return Err(CompileError::LlvmError(format!(
                        "codegen does not yet support extern parameter type '{}' for '{}' \
                         (param {}). Use `mimi run` (interpreter) for JSON-serialized FFI, \
                         or convert to a #[repr(C)] record type.",
                        ty, ef.name, i
                    )));
                }
            }
            if let crate::ffi::contract::FfiRetContract::Unsupported(ty) = &contract.ret {
                return Err(CompileError::LlvmError(format!(
                    "codegen does not yet support extern return type '{}' for '{}'. \
                     Use `mimi run` (interpreter) for JSON-serialized FFI, \
                     or convert to a #[repr(C)] record type.",
                    ty, ef.name
                )));
            }

            // Store extern func definition + ABI for lazy code generation
            self.extern_func_defs.insert(ef.name.clone(), ef.clone());
            self.extern_block_abis
                .insert(ef.name.clone(), block.abi.clone());
        }
        Ok(())
    }

    /// Lazily generate the LLVM wrapper function and extern declaration for
    /// an extern function that was previously registered by register_extern_block.
    /// Called from compile_call when an extern function is actually invoked.
    pub(in crate::codegen) fn generate_extern_fn(&mut self, name: &str) -> MimiResult<()> {
        let ef = self
            .extern_func_defs
            .get(name)
            .ok_or_else(|| {
                CompileError::LlvmError(format!("extern function '{}' not registered", name))
            })?
            .clone();
        let abi = self
            .extern_block_abis
            .get(name)
            .ok_or_else(|| CompileError::LlvmError(format!("extern ABI for '{}' not found", name)))?
            .clone();

        let sig = self.build_extern_signature(&ef, &abi)?;
        let (extern_fn, wrapper_fn) = self.declare_extern_and_wrapper(&ef, &sig, &abi)?;

        let entry = self.context.append_basic_block(wrapper_fn, "entry");
        let previous_block = self.builder.get_insert_block();
        self.builder.position_at_end(entry);

        // Phase 1: Retain c_shared/c_borrow/c_borrow_mut params before C call
        let shared_params = self.emit_shared_param_retains(&ef, wrapper_fn)?;

        // Phase 2: Check cap params
        self.emit_cap_checks(&ef, wrapper_fn)?;

        // Phase 3: Check requires contract before C call
        if self.verify_contracts {
            self.emit_requires_check(&ef, wrapper_fn)?;
        }

        // Phase 4n: Install #[no_panic] crash-recovery signal handlers if requested
        if ef.no_panic {
            self.emit_no_panic_install()?;
        }

        // Phase 4: Build wrapper args, converting Mimi types to C ABI.
        let (mut wrapper_args, json_strings) = self.emit_arg_conversions(&ef, wrapper_fn, &sig)?;

        // For complex repr(C) record returns, allocate an sret alloca and pass
        // its pointer as the first argument. The extern function returns void
        // and writes the result into this alloca.
        let sret_alloca = if sig.is_complex_reprc_ret {
            if let Some(c_struct_ty) = &sig.c_struct_ty {
                let alloca = self.build_alloca(*c_struct_ty, &format!("{}_sret", ef.name))?;
                wrapper_args.insert(0, BasicMetadataValueEnum::PointerValue(alloca));
                Some(alloca)
            } else {
                return Err(CompileError::LlvmError(format!(
                    "sret: no c_struct_ty for '{}'",
                    ef.name
                )));
            }
        } else {
            None
        };

        let call = self.build_call(extern_fn, &wrapper_args, "extern_call")?;

        // Free temporary allocations, release shared params, restore handlers.
        self.emit_ffi_cleanup(&ef, wrapper_fn, &json_strings, &shared_params)?;

        // Phase 5: Check ensures contract after C call
        if self.verify_contracts {
            self.emit_ensures_check(&ef, wrapper_fn, &call)?;
        }

        // Phase 6: Return
        self.emit_extern_return(&ef, wrapper_fn, &call, &sig, sret_alloca)?;

        if let Some(block) = previous_block {
            self.builder.position_at_end(block);
        }
        Ok(())
    }

    // -------------------------------------------------------------------------
    // Extern wrapper helpers
    // -------------------------------------------------------------------------

    /// Compute LLVM function types for the Mimi wrapper and the raw extern symbol.
    fn build_extern_signature(
        &self,
        ef: &crate::ast::ExternFunc,
        _abi: &str,
    ) -> MimiResult<ExternFnSignature<'ctx>> {
        let list_struct_sty = self.context.struct_type(
            &[
                BasicTypeEnum::IntType(self.context.i64_type()),
                BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default())),
            ],
            false,
        );
        let list_struct_ty = BasicTypeEnum::StructType(list_struct_sty);
        let list_ptr_ty =
            BasicTypeEnum::PointerType(self.context.ptr_type(inkwell::AddressSpace::default()));

        let mut param_tys = Vec::new();
        for p in &ef.params {
            let ty = match &p.ty {
                crate::ast::Type::Name(n, _)
                    if n == "List"
                        || (self.record_type_names.contains(n.as_str())
                            && !self.repr_c_record_names.contains(n.as_str())) =>
                {
                    list_ptr_ty
                }
                // For the wrapper function (called from internal Mimi code),
                // use Mimi's internal types (i32 → i64, etc.) so the call site
                // matches the wrapper signature without LLVM type mismatches.
                _ => match &p.ty {
                    crate::ast::Type::Name(n, _) if n == "i32" => {
                        BasicTypeEnum::IntType(self.context.i64_type())
                    }
                    crate::ast::Type::Name(n, _) if n == "bool" => {
                        BasicTypeEnum::IntType(self.context.i8_type())
                    }
                    _ => self.type_to_llvm_for_extern(&p.ty)?,
                },
            };
            param_tys.push(types::basic_to_metadata(self.context, ty));
        }

        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let mut extern_param_tys: Vec<BasicMetadataTypeEnum<'ctx>> = Vec::new();
        for p in &ef.params {
            let llvm_ty = match &p.ty {
                crate::ast::Type::Name(n, _)
                    if n == "string"
                        || n == "List"
                        || (self.record_type_names.contains(n.as_str())
                            && !self.repr_c_record_names.contains(n.as_str())) =>
                {
                    BasicMetadataTypeEnum::PointerType(i8_ptr_ty)
                }
                // F7: Tuple extern params are serialized to JSON, passed as i8*
                crate::ast::Type::Tuple(_) => BasicMetadataTypeEnum::PointerType(i8_ptr_ty),
                // For repr(C) records:
                // - Simple (all i32, ≤2 fields): pack into i64 to match Rust's C ABI.
                // - Complex (mixed types / >2 fields): pass by pointer.
                crate::ast::Type::Name(n, _) if self.repr_c_record_names.contains(n.as_str()) => {
                    if let Some(td) = self.type_defs.get(n.as_str()) {
                        if let TypeDefKind::Record(ref fields) = td.kind {
                            if types::is_simple_reprc_record(fields) {
                                BasicMetadataTypeEnum::IntType(self.context.i64_type())
                            } else {
                                BasicMetadataTypeEnum::PointerType(i8_ptr_ty)
                            }
                        } else {
                            BasicMetadataTypeEnum::IntType(self.context.i64_type())
                        }
                    } else {
                        BasicMetadataTypeEnum::IntType(self.context.i64_type())
                    }
                }
                _ => {
                    let ty = self.type_to_llvm_for_extern(&p.ty)?;
                    types::basic_to_metadata(self.context, ty)
                }
            };
            extern_param_tys.push(llvm_ty);
        }

        let wrapper_ret_ty = match &ef.ret {
            Some(ty) => {
                if matches!(ty, crate::ast::Type::Name(n, _) if n == "List" || (self.record_type_names.contains(n.as_str()) && !self.repr_c_record_names.contains(n.as_str())))
                {
                    list_struct_ty
                // F7: Tuple return — wrapper returns the LLVM struct type
                } else if matches!(ty, crate::ast::Type::Tuple(_)) {
                    types::mimi_type_to_llvm(self.context, ty).ok_or_else(|| {
                        CompileError::TypeMismatch(format!(
                            "Tuple type '{}' could not be mapped to LLVM struct",
                            crate::core::fmt_type(ty)
                        ))
                    })?
                } else {
                    self.type_to_llvm_for_extern(ty)?
                }
            }
            None => BasicTypeEnum::IntType(self.context.i64_type()),
        };

        let is_complex_reprc_ret = ef.ret.as_ref().is_some_and(|ty| {
            if let crate::ast::Type::Name(n, _) = ty {
                self.repr_c_record_names.contains(n.as_str())
                    && self.type_defs.get(n.as_str()).is_some_and(|td| {
                        matches!(&td.kind, TypeDefKind::Record(fields) if !types::is_simple_reprc_record(fields))
                    })
            } else {
                false
            }
        });
        let c_struct_ty = if is_complex_reprc_ret {
            ef.ret.as_ref().and_then(|ty| {
                if let crate::ast::Type::Name(n, _) = ty {
                    self.type_defs.get(n.as_str()).and_then(|td| {
                        if let TypeDefKind::Record(ref fields) = td.kind {
                            self.c_layout_return_type(n, fields)
                                .ok()
                                .and_then(|t| match t {
                                    BasicTypeEnum::StructType(s) => Some(s),
                                    _ => None,
                                })
                        } else {
                            None
                        }
                    })
                } else {
                    None
                }
            })
        } else {
            None
        };

        // For complex repr(C) records, extern_ret_ty is not used (fn_type is
        // built separately with void return type). Set a dummy value here.
        let extern_ret_ty = if is_complex_reprc_ret {
            BasicTypeEnum::IntType(self.context.i64_type())
        } else {
            match &ef.ret {
                Some(ty) => {
                    if matches!(ty, crate::ast::Type::Name(n, _) if n == "List" || (self.record_type_names.contains(n.as_str()) && !self.repr_c_record_names.contains(n.as_str())))
                        || matches!(ty, crate::ast::Type::Tuple(_))
                        || matches!(ty, crate::ast::Type::Name(n, _) if n == "string")
                    {
                        BasicTypeEnum::PointerType(i8_ptr_ty)
                    } else if let crate::ast::Type::Name(n, _) = ty {
                        if self.repr_c_record_names.contains(n.as_str()) {
                            if let Some(td) = self.type_defs.get(n.as_str()) {
                                if let TypeDefKind::Record(ref fields) = td.kind {
                                    if types::is_simple_reprc_record(fields) {
                                        BasicTypeEnum::IntType(self.context.i64_type())
                                    } else {
                                        self.c_layout_return_type(n, fields)?
                                    }
                                } else {
                                    BasicTypeEnum::IntType(self.context.i64_type())
                                }
                            } else {
                                BasicTypeEnum::IntType(self.context.i64_type())
                            }
                        } else {
                            self.type_to_llvm_for_extern(ty)?
                        }
                    } else {
                        self.type_to_llvm_for_extern(ty)?
                    }
                }
                None => BasicTypeEnum::IntType(self.context.i64_type()),
            }
        };

        // For complex repr(C) record returns, add an sret pointer parameter.
        let i8_ptr_ty_sret = self.context.ptr_type(inkwell::AddressSpace::default());
        let mut sret_param_tys = extern_param_tys.clone();
        if is_complex_reprc_ret {
            sret_param_tys.insert(0, BasicMetadataTypeEnum::PointerType(i8_ptr_ty_sret));
        }

        let is_variadic = ef.variadic;

        let wrapper_fn_type = self.fn_type_for_ret(wrapper_ret_ty, &param_tys, is_variadic);
        let extern_fn_type = if is_complex_reprc_ret {
            self.context
                .void_type()
                .fn_type(&sret_param_tys, is_variadic)
        } else {
            self.fn_type_for_ret(extern_ret_ty, &sret_param_tys, is_variadic)
        };

        Ok(ExternFnSignature {
            list_struct_sty,
            list_struct_ty,
            wrapper_ret_ty,
            wrapper_fn_type,
            extern_fn_type,
            is_complex_reprc_ret,
            c_struct_ty,
        })
    }

    /// Build a `FunctionType` from an LLVM return type and parameter types.
    fn fn_type_for_ret(
        &self,
        ret: BasicTypeEnum<'ctx>,
        params: &[BasicMetadataTypeEnum<'ctx>],
        variadic: bool,
    ) -> FunctionType<'ctx> {
        match ret {
            BasicTypeEnum::IntType(t) => t.fn_type(params, variadic),
            BasicTypeEnum::FloatType(t) => t.fn_type(params, variadic),
            BasicTypeEnum::PointerType(t) => t.fn_type(params, variadic),
            BasicTypeEnum::StructType(t) => t.fn_type(params, variadic),
            BasicTypeEnum::ArrayType(t) => t.fn_type(params, variadic),
            _ => self.context.i64_type().fn_type(params, variadic),
        }
    }

    /// Declare the raw extern symbol and the Mimi wrapper function.
    fn declare_extern_and_wrapper(
        &self,
        ef: &crate::ast::ExternFunc,
        sig: &ExternFnSignature<'ctx>,
        abi: &str,
    ) -> MimiResult<(
        inkwell::values::FunctionValue<'ctx>,
        inkwell::values::FunctionValue<'ctx>,
    )> {
        let extern_name = format!("__mimi_extern_{}", ef.name);
        let extern_fn = self.module.add_function(
            &extern_name,
            sig.extern_fn_type,
            Some(inkwell::module::Linkage::External),
        );
        let cc = crate::ffi::abi_to_llvm_call_conv(abi);
        extern_fn.set_call_conventions(cc);
        let wrapper_fn = self.module.add_function(
            &ef.name,
            sig.wrapper_fn_type,
            Some(inkwell::module::Linkage::Internal),
        );
        Ok((extern_fn, wrapper_fn))
    }

    /// Phase 1: retain c_shared/c_borrow/c_borrow_mut params before the C call.
    fn emit_shared_param_retains(
        &self,
        ef: &crate::ast::ExternFunc,
        wrapper_fn: inkwell::values::FunctionValue<'ctx>,
    ) -> MimiResult<Vec<(usize, BasicValueEnum<'ctx>)>> {
        let mut shared_params = Vec::new();
        let i64_ty = self.context.i64_type();
        for (i, p) in ef.params.iter().enumerate() {
            if matches!(p.ty, crate::ast::Type::CShared(_))
                || matches!(p.ty, crate::ast::Type::CBorrow(_))
                || matches!(p.ty, crate::ast::Type::CBorrowMut(_))
            {
                let param = wrapper_fn
                    .get_nth_param(i as u32)
                    .ok_or_else(|| CompileError::LlvmError(format!("missing param {}", i)))?;
                if let Some(retain_fn) = self.module.get_function("mimi_shared_retain") {
                    let param_i64 = match param {
                        BasicValueEnum::IntValue(iv) => iv,
                        BasicValueEnum::PointerValue(pv) => {
                            self.build_ptr_to_int(pv, i64_ty, &format!("ptr_to_i64_{}", i))?
                        }
                        _ => {
                            return Err(CompileError::TypeMismatch(format!(
                                "c_shared/c_borrow param {} must be pointer or int",
                                i
                            )))
                        }
                    };
                    self.build_call(
                        retain_fn,
                        &[BasicMetadataValueEnum::IntValue(param_i64)],
                        &format!("retain_{}", i),
                    )?;
                }
                shared_params.push((i, param));
            }
        }
        Ok(shared_params)
    }

    /// Phase 2: validate capability parameters, aborting the process if absent.
    fn emit_cap_checks(
        &mut self,
        ef: &crate::ast::ExternFunc,
        wrapper_fn: inkwell::values::FunctionValue<'ctx>,
    ) -> MimiResult<()> {
        for (i, p) in ef.params.iter().enumerate() {
            if let crate::ast::Type::Cap(cap_name) = &p.ty {
                let param = wrapper_fn
                    .get_nth_param(i as u32)
                    .ok_or_else(|| CompileError::LlvmError(format!("missing param {}", i)))?;
                if let Some(check_fn) = self.module.get_function("mimi_cap_check") {
                    let cap_name_global = self
                        .builder
                        .build_global_string_ptr(
                            &format!("{}\0", cap_name),
                            &format!("cap_name_{}", i),
                        )
                        .map_err(|e| {
                            CompileError::LlvmError(format!("string global error: {}", e))
                        })?;
                    let cap_name_ptr = cap_name_global.as_pointer_value();
                    let check_result = self
                        .build_call(
                            check_fn,
                            &[
                                BasicMetadataValueEnum::IntValue(param.into_int_value()),
                                BasicMetadataValueEnum::PointerValue(cap_name_ptr),
                            ],
                            &format!("cap_check_{}", i),
                        )?
                        .try_as_basic_value_opt()
                        .ok_or_else(|| {
                            CompileError::LlvmError("cap_check returned void".to_string())
                        })?
                        .into_int_value();

                    let zero_i32 = self.context.i32_type().const_int(0, false);
                    let is_valid = self
                        .builder
                        .build_int_compare(
                            inkwell::IntPredicate::NE,
                            check_result,
                            zero_i32,
                            "cap_valid",
                        )
                        .map_err(|e| CompileError::LlvmError(format!("compare error: {}", e)))?;

                    let ok_bb = self
                        .context
                        .append_basic_block(wrapper_fn, &format!("cap_ok_{}", i));
                    let fail_bb = self
                        .context
                        .append_basic_block(wrapper_fn, &format!("cap_fail_{}", i));
                    self.build_cond_br(is_valid, ok_bb, fail_bb)?;

                    self.builder.position_at_end(fail_bb);
                    let exit_fn = match self.module.get_function("exit") {
                        Some(f) => f,
                        None => {
                            let exit_ty = self.context.void_type().fn_type(
                                &[BasicMetadataTypeEnum::IntType(self.context.i32_type())],
                                false,
                            );
                            self.module.add_function("exit", exit_ty, None)
                        }
                    };
                    self.build_call(
                        exit_fn,
                        &[BasicMetadataValueEnum::IntValue(
                            self.context.i32_type().const_int(1, false),
                        )],
                        "cap_fail_exit",
                    )?;
                    self.build_br(ok_bb)?;
                    self.builder.position_at_end(ok_bb);
                }
            }
        }
        Ok(())
    }

    /// Build a map of parameter names to alloca-backed VarEntries for contract checking.
    fn contract_vars_from_params(
        &self,
        ef: &crate::ast::ExternFunc,
        wrapper_fn: inkwell::values::FunctionValue<'ctx>,
    ) -> MimiResult<HashMap<String, VarEntry<'ctx>>> {
        let mut contract_vars: HashMap<String, VarEntry<'ctx>> = HashMap::new();
        for (i, p) in ef.params.iter().enumerate() {
            let param = wrapper_fn
                .get_nth_param(i as u32)
                .ok_or_else(|| CompileError::LlvmError(format!("missing param {}", i)))?;
            let llvm_ty = param.get_type();
            let alloca = self.build_alloca(llvm_ty, &p.name)?;
            self.build_store(alloca, param)?;
            contract_vars.insert(p.name.clone(), (alloca, llvm_ty));
        }
        Ok(contract_vars)
    }

    /// Phase 3: emit the requires contract as a runtime assertion before the C call.
    fn emit_requires_check(
        &mut self,
        ef: &crate::ast::ExternFunc,
        wrapper_fn: inkwell::values::FunctionValue<'ctx>,
    ) -> MimiResult<()> {
        if let Some(req_expr) = &ef.requires {
            let contract_vars = self.contract_vars_from_params(ef, wrapper_fn)?;
            self.compile_contract_assert(
                req_expr,
                &contract_vars,
                &format!("requires violation in extern '{}'", ef.name),
            )?;
        }
        Ok(())
    }

    /// Install #[no_panic] crash-recovery signal handlers before the C call.
    fn emit_no_panic_install(&self) -> MimiResult<()> {
        let void_fty = self.context.void_type().fn_type(&[], false);
        let install_fn = match self.module.get_function("mimi_install_no_panic_handlers") {
            Some(f) => f,
            None => self
                .module
                .add_function("mimi_install_no_panic_handlers", void_fty, None),
        };
        self.build_call(install_fn, &[], "no_panic_install")?;
        Ok(())
    }

    /// Restore #[no_panic] signal handlers after the C call.
    fn emit_no_panic_restore(&self) -> MimiResult<()> {
        let void_fty = self.context.void_type().fn_type(&[], false);
        let restore_fn = match self.module.get_function("mimi_restore_no_panic_handlers") {
            Some(f) => f,
            None => self
                .module
                .add_function("mimi_restore_no_panic_handlers", void_fty, None),
        };
        self.build_call(restore_fn, &[], "no_panic_restore")?;
        Ok(())
    }

    /// Phase 4: convert each Mimi argument to the representation expected by C.
    fn emit_arg_conversions(
        &mut self,
        ef: &crate::ast::ExternFunc,
        wrapper_fn: inkwell::values::FunctionValue<'ctx>,
        sig: &ExternFnSignature<'ctx>,
    ) -> MimiResult<(Vec<BasicMetadataValueEnum<'ctx>>, Vec<PointerValue<'ctx>>)> {
        let mut wrapper_args = Vec::new();
        let mut json_strings = Vec::new();

        for (i, p) in ef.params.iter().enumerate() {
            let param = wrapper_fn
                .get_nth_param(i as u32)
                .ok_or_else(|| CompileError::LlvmError(format!("missing param {}", i)))?;
            match &p.ty {
                crate::ast::Type::Name(n, _) if n == "string" => {
                    wrapper_args.push(self.emit_string_arg_conversion(param, i)?);
                }
                crate::ast::Type::Name(n, _)
                    if n == "List"
                        || (self.record_type_names.contains(n.as_str())
                            && !self.repr_c_record_names.contains(n.as_str())) =>
                {
                    let json_val = self.emit_list_or_record_arg_conversion(param, &p.ty, i, sig)?;
                    json_strings.push(json_val);
                    wrapper_args.push(BasicMetadataValueEnum::PointerValue(json_val));
                }
                crate::ast::Type::Tuple(elems) => {
                    let json_val = self.emit_tuple_arg_conversion(param, elems, i)?;
                    json_strings.push(json_val);
                    wrapper_args.push(BasicMetadataValueEnum::PointerValue(json_val));
                }
                _ => {
                    if let Some(arg) = self.emit_reprc_record_arg_conversion(param, &p.ty, i)? {
                        wrapper_args.push(arg);
                    } else {
                        // If the Mimi internal type (e.g. i64 for i32) differs from the
                        // extern C type (e.g. i32 for i32), truncate/extend as needed.
                        let converted = if let crate::ast::Type::Name(n, _) = &p.ty {
                            match n.as_str() {
                                "i32" => {
                                    if let BasicValueEnum::IntValue(iv) = param {
                                        BasicValueEnum::IntValue(
                                            self.builder
                                                .build_int_truncate(
                                                    iv,
                                                    self.context.i32_type(),
                                                    &format!("trunc_i64_i32_{}", i),
                                                )
                                                .map_err(|e| {
                                                    CompileError::LlvmError(format!(
                                                        "trunc error: {}",
                                                        e
                                                    ))
                                                })?,
                                        )
                                    } else {
                                        param
                                    }
                                }
                                "bool" => {
                                    if let BasicValueEnum::IntValue(iv) = param {
                                        BasicValueEnum::IntValue(
                                            self.builder
                                                .build_int_truncate(
                                                    iv,
                                                    self.context.i8_type(),
                                                    &format!("trunc_i64_i8_{}", i),
                                                )
                                                .map_err(|e| {
                                                    CompileError::LlvmError(format!(
                                                        "trunc error: {}",
                                                        e
                                                    ))
                                                })?,
                                        )
                                    } else {
                                        param
                                    }
                                }
                                _ => param,
                            }
                        } else {
                            param
                        };
                        wrapper_args.push(match converted {
                            BasicValueEnum::IntValue(v) => BasicMetadataValueEnum::IntValue(v),
                            BasicValueEnum::FloatValue(v) => BasicMetadataValueEnum::FloatValue(v),
                            BasicValueEnum::PointerValue(v) => {
                                BasicMetadataValueEnum::PointerValue(v)
                            }
                            BasicValueEnum::StructValue(v) => {
                                BasicMetadataValueEnum::StructValue(v)
                            }
                            BasicValueEnum::ArrayValue(v) => BasicMetadataValueEnum::ArrayValue(v),
                            BasicValueEnum::VectorValue(v) => {
                                BasicMetadataValueEnum::VectorValue(v)
                            }
                            BasicValueEnum::ScalableVectorValue(_) => {
                                BasicMetadataValueEnum::IntValue(
                                    self.context.i64_type().const_int(0, false),
                                )
                            }
                        });
                    }
                }
            }
        }
        Ok((wrapper_args, json_strings))
    }

    /// Convert a Mimi string argument to a bare `i8*` pointer.
    fn emit_string_arg_conversion(
        &self,
        param: BasicValueEnum<'ctx>,
        i: usize,
    ) -> MimiResult<BasicMetadataValueEnum<'ctx>> {
        let struct_val = param.into_struct_value();
        let ptr = self
            .builder
            .build_extract_value(struct_val, 0, &format!("str_ptr_{}", i))
            .map_err(|e| CompileError::LlvmError(format!("extract value error: {}", e)))?;
        match ptr {
            BasicValueEnum::PointerValue(pv) => Ok(BasicMetadataValueEnum::PointerValue(pv)),
            _ => Err(CompileError::TypeMismatch(format!(
                "string param {}: expected pointer from struct field 0",
                i
            ))),
        }
    }

    /// Convert a List/Record argument to a freshly allocated JSON string (i8*).
    fn emit_list_or_record_arg_conversion(
        &self,
        param: BasicValueEnum<'ctx>,
        ty: &crate::ast::Type,
        i: usize,
        sig: &ExternFnSignature<'ctx>,
    ) -> MimiResult<PointerValue<'ctx>> {
        let list_ptr = param.into_pointer_value();
        let len_gep = self
            .gep()
            .build_struct_gep(
                sig.list_struct_sty,
                list_ptr,
                0,
                &format!("list_len_gep_{}", i),
            )
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let data_gep = self
            .gep()
            .build_struct_gep(
                sig.list_struct_sty,
                list_ptr,
                1,
                &format!("list_data_gep_{}", i),
            )
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        let len_val =
            self.build_load(self.context.i64_type(), len_gep, &format!("list_len_{}", i))?;
        let data_ptr_val = self.build_load(
            self.context.ptr_type(inkwell::AddressSpace::default()),
            data_gep,
            &format!("list_data_{}", i),
        )?;
        let data_pv = match data_ptr_val {
            BasicValueEnum::PointerValue(pv) => pv,
            _ => {
                return Err(CompileError::TypeMismatch(format!(
                    "list data ptr {}: expected pointer",
                    i
                )))
            }
        };
        let len_iv = match len_val {
            BasicValueEnum::IntValue(iv) => iv,
            _ => {
                return Err(CompileError::TypeMismatch(format!(
                    "list len {}: expected int",
                    i
                )))
            }
        };

        let elem_tag = elem_type_tag(ty);
        let elem_tag_iv = self.context.i64_type().const_int(elem_tag as u64, false);

        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let serialize_fn = if let Some(f) = self.module.get_function("mimi_json_serialize") {
            f
        } else {
            let fn_ty = i8_ptr_ty.fn_type(
                &[
                    BasicMetadataTypeEnum::PointerType(i8_ptr_ty),
                    BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                    BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                ],
                false,
            );
            self.module.add_function(
                "mimi_json_serialize",
                fn_ty,
                Some(inkwell::module::Linkage::External),
            )
        };

        let json_val = self
            .build_call(
                serialize_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(data_pv),
                    BasicMetadataValueEnum::IntValue(len_iv),
                    BasicMetadataValueEnum::IntValue(elem_tag_iv),
                ],
                &format!("json_ser_{}", i),
            )?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("json serialize returned void".to_string()))?
            .into_pointer_value();
        Ok(json_val)
    }

    /// Convert a Tuple argument to a freshly allocated JSON string (i8*).
    fn emit_tuple_arg_conversion(
        &self,
        param: BasicValueEnum<'ctx>,
        elems: &[crate::ast::Type],
        i: usize,
    ) -> MimiResult<PointerValue<'ctx>> {
        let n = elems.len();
        let i32_ty = self.context.i32_type();
        let zero = i32_ty.const_int(0, false);
        let i64_ty = self.context.i64_type();

        let struct_val = param.into_struct_value();
        let struct_ty = struct_val.get_type();
        let alloca = self.build_alloca(
            BasicTypeEnum::StructType(struct_ty),
            &format!("tuple_alloca_{}", i),
        )?;
        self.build_store(alloca, param)?;

        let arr_ty = i64_ty.array_type(n as u32);
        let vals_alloca = self.build_alloca(
            BasicTypeEnum::ArrayType(arr_ty),
            &format!("tuple_vals_{}", i),
        )?;
        let tys_alloca = self.build_alloca(
            BasicTypeEnum::ArrayType(arr_ty),
            &format!("tuple_tys_{}", i),
        )?;

        for (ei, elem_ty) in elems.iter().enumerate() {
            let idx = i32_ty.const_int(ei as u64, false);
            let elem_raw = self
                .builder
                .build_extract_value(struct_val, ei as u32, &format!("tuple_elem_{}_{}", i, ei))
                .map_err(|e| CompileError::LlvmError(format!("extract: {}", e)))?;
            let elem_i64 = self.tuple_elem_to_i64(elem_raw, i, ei)?;

            let val_gep = self
                .gep()
                .build_gep(
                    i64_ty,
                    vals_alloca,
                    &[zero, idx],
                    &format!("tv_gep_{}_{}", i, ei),
                )
                .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
            self.build_store(val_gep, elem_i64)?;

            let tag_val = elem_type_tag(elem_ty);
            let tag_i64 = i64_ty.const_int(tag_val as u64, false);
            let ty_gep = self
                .gep()
                .build_gep(
                    i64_ty,
                    tys_alloca,
                    &[zero, idx],
                    &format!("tt_gep_{}_{}", i, ei),
                )
                .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
            self.build_store(ty_gep, tag_i64)?;
        }

        let n_i64 = i64_ty.const_int(n as u64, false);
        let i64_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let serialize_fn = if let Some(f) = self.module.get_function("mimi_tuple_serialize") {
            f
        } else {
            let fn_ty = self
                .context
                .ptr_type(inkwell::AddressSpace::default())
                .fn_type(
                    &[
                        BasicMetadataTypeEnum::PointerType(i64_ptr_ty),
                        BasicMetadataTypeEnum::IntType(i64_ty),
                        BasicMetadataTypeEnum::PointerType(i64_ptr_ty),
                    ],
                    false,
                );
            self.module.add_function(
                "mimi_tuple_serialize",
                fn_ty,
                Some(inkwell::module::Linkage::External),
            )
        };

        let json_val = self
            .build_call(
                serialize_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(vals_alloca),
                    BasicMetadataValueEnum::IntValue(n_i64),
                    BasicMetadataValueEnum::PointerValue(tys_alloca),
                ],
                &format!("tuple_ser_{}", i),
            )?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("tuple serialize returned void".to_string()))?
            .into_pointer_value();
        Ok(json_val)
    }

    /// Coerce a single tuple element into a generic i64 payload.
    fn tuple_elem_to_i64(
        &self,
        elem_raw: BasicValueEnum<'ctx>,
        i: usize,
        ei: usize,
    ) -> MimiResult<inkwell::values::IntValue<'ctx>> {
        let i64_ty = self.context.i64_type();
        match elem_raw {
            BasicValueEnum::IntValue(iv) => Ok(iv),
            BasicValueEnum::FloatValue(fv) => {
                let f_alloca = self.build_alloca(
                    BasicTypeEnum::FloatType(self.context.f64_type()),
                    &format!("tf_{}_{}", i, ei),
                )?;
                self.build_store(f_alloca, fv)?;
                let cast = self
                    .builder
                    .build_pointer_cast(
                        f_alloca,
                        self.context.ptr_type(inkwell::AddressSpace::default()),
                        &format!("f_cast_{}_{}", i, ei),
                    )
                    .map_err(|e| CompileError::LlvmError(format!("cast: {}", e)))?;
                Ok(self
                    .build_load(i64_ty, cast, &format!("f_bits_{}_{}", i, ei))?
                    .into_int_value())
            }
            BasicValueEnum::PointerValue(pv) => {
                let i64_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                let cast = self
                    .builder
                    .build_pointer_cast(pv, i64_ptr_ty, &format!("p_cast_{}_{}", i, ei))
                    .map_err(|e| CompileError::LlvmError(format!("cast: {}", e)))?;
                Ok(self
                    .build_load(i64_ty, cast, &format!("p_val_{}_{}", i, ei))?
                    .into_int_value())
            }
            BasicValueEnum::StructValue(sv) => {
                let s_ty = sv.get_type();
                let s_alloca = self
                    .build_alloca(BasicTypeEnum::StructType(s_ty), &format!("ts_{}_{}", i, ei))?;
                self.build_store(s_alloca, sv)?;
                let cast = self
                    .builder
                    .build_pointer_cast(
                        s_alloca,
                        self.context.ptr_type(inkwell::AddressSpace::default()),
                        &format!("s_cast_{}_{}", i, ei),
                    )
                    .map_err(|e| CompileError::LlvmError(format!("cast: {}", e)))?;
                Ok(self
                    .build_load(i64_ty, cast, &format!("s_val_{}_{}", i, ei))?
                    .into_int_value())
            }
            _ => Ok(i64_ty.const_int(0, false)),
        }
    }

    /// Convert a #[repr(C)] record argument to its C ABI representation.
    /// Returns `None` when the parameter is not a repr(C) record.
    fn emit_reprc_record_arg_conversion(
        &self,
        param: BasicValueEnum<'ctx>,
        ty: &crate::ast::Type,
        i: usize,
    ) -> MimiResult<Option<BasicMetadataValueEnum<'ctx>>> {
        let BasicValueEnum::StructValue(sv) = param else {
            return Ok(None);
        };
        let crate::ast::Type::Name(n, _) = ty else {
            return Ok(None);
        };
        if !self.repr_c_record_names.contains(n.as_str()) {
            return Ok(None);
        }
        let Some(td) = self.type_defs.get(n.as_str()) else {
            return Ok(None);
        };
        let TypeDefKind::Record(fields) = &td.kind else {
            return Ok(None);
        };

        if types::is_simple_reprc_record(fields) {
            return Ok(Some(self.emit_simple_reprc_arg(sv, n, fields, i)?));
        }
        Ok(Some(self.emit_complex_reprc_arg(sv, n, fields, i)?))
    }

    /// Pack a simple #[repr(C)] record (all i32, ≤2 fields) into a single i64.
    fn emit_simple_reprc_arg(
        &self,
        sv: inkwell::values::StructValue<'ctx>,
        n: &str,
        fields: &[Field],
        i: usize,
    ) -> MimiResult<BasicMetadataValueEnum<'ctx>> {
        let _ = i;
        let i64_ty = self.context.i64_type();
        let i32_ty = self.context.i32_type();
        let mut packed = i64_ty.const_int(0, false);
        for (fi, f) in fields.iter().enumerate() {
            let raw_val = self
                .builder
                .build_extract_value(sv, fi as u32, &format!("{}_{}_raw", n, f.name))
                .map_err(|e| CompileError::LlvmError(format!("extract field: {}", e)))?;
            let truncated_i32 = match raw_val {
                BasicValueEnum::IntValue(iv) => self
                    .builder
                    .build_int_truncate(iv, i32_ty, &format!("{}_{}_trunc", n, f.name))
                    .map_err(|e| CompileError::LlvmError(format!("trunc: {}", e)))?,
                _ => {
                    return Err(CompileError::TypeMismatch(format!(
                        "repr(C) field {} expected i32 but got non-integer",
                        f.name
                    )))
                }
            };
            let zext = self
                .builder
                .build_int_z_extend(truncated_i32, i64_ty, &format!("{}_{}_zext", n, f.name))
                .map_err(|e| CompileError::LlvmError(format!("zext: {}", e)))?;
            if fi == 0 {
                packed = zext;
            } else {
                let shifted = self
                    .builder
                    .build_left_shift(
                        zext,
                        i64_ty.const_int((fi * 32) as u64, false),
                        &format!("{}_{}_shifted", n, f.name),
                    )
                    .map_err(|e| CompileError::LlvmError(format!("shift: {}", e)))?;
                packed = self
                    .builder
                    .build_or(packed, shifted, &format!("{}_{}_packed", n, f.name))
                    .map_err(|e| CompileError::LlvmError(format!("or: {}", e)))?;
            }
        }
        Ok(BasicMetadataValueEnum::IntValue(packed))
    }

    /// Build a C-layout struct on the stack for a complex #[repr(C)] record.
    fn emit_complex_reprc_arg(
        &self,
        sv: inkwell::values::StructValue<'ctx>,
        n: &str,
        fields: &[Field],
        i: usize,
    ) -> MimiResult<BasicMetadataValueEnum<'ctx>> {
        let _ = i;
        let mut c_field_tys = Vec::new();
        for f in fields {
            let c_ty = types::mimi_type_to_llvm_extern(self.context, &f.ty).ok_or_else(|| {
                CompileError::LlvmError(format!(
                    "cannot map field '{}' type '{}' to C-compatible LLVM type",
                    f.name,
                    crate::core::fmt_type(&f.ty)
                ))
            })?;
            c_field_tys.push(c_ty);
        }
        let c_struct_ty = self.context.struct_type(&c_field_tys, false);
        let c_alloca = self.build_alloca(
            BasicTypeEnum::StructType(c_struct_ty),
            &format!("c_struct_{}", n),
        )?;
        for (fi, f) in fields.iter().enumerate() {
            let raw_val = self
                .builder
                .build_extract_value(sv, fi as u32, &format!("{}_{}_raw", n, f.name))
                .map_err(|e| CompileError::LlvmError(format!("extract field: {}", e)))?;
            let c_val: BasicValueEnum = match &f.ty {
                crate::ast::Type::Name(tn, _) if tn == "i32" => match raw_val {
                    BasicValueEnum::IntValue(iv) => {
                        let truncated = self
                            .builder
                            .build_int_truncate(
                                iv,
                                self.context.i32_type(),
                                &format!("{}_{}_trunc", n, f.name),
                            )
                            .map_err(|e| CompileError::LlvmError(format!("trunc: {}", e)))?;
                        BasicValueEnum::IntValue(truncated)
                    }
                    _ => {
                        return Err(CompileError::TypeMismatch(format!(
                            "repr(C) field {} expected i32 but got non-integer",
                            f.name
                        )))
                    }
                },
                crate::ast::Type::Name(tn, _) if tn == "bool" => match raw_val {
                    BasicValueEnum::IntValue(iv) => {
                        let zext = self
                            .builder
                            .build_int_z_extend(
                                iv,
                                self.context.i8_type(),
                                &format!("{}_{}_bool_ext", n, f.name),
                            )
                            .map_err(|e| CompileError::LlvmError(format!("zext: {}", e)))?;
                        BasicValueEnum::IntValue(zext)
                    }
                    _ => {
                        return Err(CompileError::TypeMismatch(format!(
                            "repr(C) field {} expected bool but got non-integer",
                            f.name
                        )))
                    }
                },
                _ => raw_val,
            };
            let gep = self
                .gep()
                .build_struct_gep(
                    c_struct_ty,
                    c_alloca,
                    fi as u32,
                    &format!("{}_{}_gep", n, f.name),
                )
                .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
            self.build_store(gep, c_val)?;
        }
        let c_ptr = self
            .builder
            .build_pointer_cast(
                c_alloca,
                self.context.ptr_type(inkwell::AddressSpace::default()),
                &format!("c_struct_ptr_{}", n),
            )
            .map_err(|e| CompileError::LlvmError(format!("ptr cast: {}", e)))?;
        Ok(BasicMetadataValueEnum::PointerValue(c_ptr))
    }

    /// Free temporary JSON strings, release shared passport params, and restore no_panic handlers.
    fn emit_ffi_cleanup(
        &self,
        ef: &crate::ast::ExternFunc,
        wrapper_fn: inkwell::values::FunctionValue<'ctx>,
        json_strings: &[PointerValue<'ctx>],
        shared_params: &[(usize, BasicValueEnum<'ctx>)],
    ) -> MimiResult<()> {
        let i64_ty = self.context.i64_type();

        for (j, json_pv) in json_strings.iter().enumerate() {
            if let Some(free_fn) = self.module.get_function("free") {
                self.build_call(
                    free_fn,
                    &[BasicMetadataValueEnum::PointerValue(*json_pv)],
                    &format!("free_json_{}", j),
                )?;
            }
        }

        for (i, _) in shared_params {
            if let Some(release_fn) = self.module.get_function("mimi_shared_release") {
                let orig_param = wrapper_fn
                    .get_nth_param(*i as u32)
                    .ok_or_else(|| CompileError::LlvmError(format!("missing param {}", i)))?;
                let param_i64 = match orig_param {
                    BasicValueEnum::IntValue(iv) => iv,
                    BasicValueEnum::PointerValue(pv) => {
                        self.build_ptr_to_int(pv, i64_ty, &format!("ptr_to_i64_rel_{}", i))?
                    }
                    _ => {
                        return Err(CompileError::TypeMismatch(format!(
                            "c_shared/c_borrow param {} must be pointer or int",
                            i
                        )))
                    }
                };
                self.build_call(
                    release_fn,
                    &[BasicMetadataValueEnum::IntValue(param_i64)],
                    &format!("release_{}", i),
                )?;
            }
        }

        if ef.no_panic {
            self.emit_no_panic_restore()?;
        }
        Ok(())
    }

    /// Phase 5: emit the ensures contract as a runtime assertion after the C call.
    fn emit_ensures_check(
        &mut self,
        ef: &crate::ast::ExternFunc,
        wrapper_fn: inkwell::values::FunctionValue<'ctx>,
        call: &CallSiteValue<'ctx>,
    ) -> MimiResult<()> {
        if let Some(ens_expr) = &ef.ensures {
            let ret_val = call_try_basic_value(call).ok_or_else(|| {
                CompileError::LlvmError("extern wrapper call did not return a value".to_string())
            })?;
            let mut contract_vars = self.contract_vars_from_params(ef, wrapper_fn)?;
            let result_ty = ret_val.get_type();
            let result_alloca = self.build_alloca(result_ty, "result")?;
            self.build_store(result_alloca, ret_val)?;
            contract_vars.insert("result".to_string(), (result_alloca, result_ty));
            self.compile_contract_assert(
                ens_expr,
                &contract_vars,
                &format!("ensures violation in extern '{}'", ef.name),
            )?;
        }
        Ok(())
    }

    /// Phase 6: convert the C return value back to the Mimi representation and return it.
    fn emit_extern_return(
        &mut self,
        ef: &crate::ast::ExternFunc,
        wrapper_fn: inkwell::values::FunctionValue<'ctx>,
        call: &CallSiteValue<'ctx>,
        sig: &ExternFnSignature<'ctx>,
        sret_alloca: Option<inkwell::values::PointerValue<'ctx>>,
    ) -> MimiResult<()> {
        let _ = wrapper_fn;
        // Check for complex repr(C) record return first.
        if sig.is_complex_reprc_ret {
            if let Some(crate::ast::Type::Name(n, _)) = &ef.ret {
                let complex_fields: Vec<Field> = self
                    .type_defs
                    .get(n.as_str())
                    .and_then(|td| {
                        if let TypeDefKind::Record(ref fields) = td.kind {
                            if !types::is_simple_reprc_record(fields) {
                                return Some(fields.clone());
                            }
                        }
                        None
                    })
                    .unwrap_or_default();
                if !complex_fields.is_empty() {
                    return self.emit_complex_reprc_return(call, n, &complex_fields, sret_alloca);
                }
            }
        }
        let is_tuple_return = ef
            .ret
            .as_ref()
            .is_some_and(|ret_ty| matches!(ret_ty, crate::ast::Type::Tuple(_)));
        let needs_json_deserialize = ef.ret.as_ref().is_some_and(|ret_ty| {
            matches!(ret_ty, crate::ast::Type::Name(n, _) if n == "List" || (self.record_type_names.contains(n.as_str()) && !self.repr_c_record_names.contains(n.as_str())))
        }) || is_tuple_return;

        if needs_json_deserialize {
            if is_tuple_return {
                self.emit_tuple_return(ef, call)?;
            } else {
                self.emit_list_record_return(ef, call, sig)?;
            }
        } else if ef
            .ret
            .as_ref()
            .is_some_and(|t| matches!(t, crate::ast::Type::Name(n, _) if n == "string"))
        {
            self.emit_string_return(call, sig)?;
        } else if sig.wrapper_fn_type.get_return_type().is_some() {
            let ret = call_try_basic_value(call).ok_or_else(|| {
                CompileError::LlvmError("extern wrapper call did not return a value".to_string())
            })?;
            self.build_return(Some(&ret))?;
        } else {
            self.build_return(None)?;
        }
        Ok(())
    }

    /// Handle complex repr(C) record return from an extern C function.
    /// Converts the C-layout struct to Mimi's internal record representation.
    fn emit_complex_reprc_return(
        &mut self,
        call: &CallSiteValue<'ctx>,
        type_name: &str,
        fields: &[Field],
        sret_alloca: Option<inkwell::values::PointerValue<'ctx>>,
    ) -> MimiResult<()> {
        let c_sv = if let Some(sret) = sret_alloca {
            // The extern function used sret: load the struct from the alloca.
            let c_struct_ty = self
                .c_layout_return_type(type_name, fields)
                .map_err(|e| CompileError::LlvmError(format!("c_layout_return_type: {}", e)))?;
            match c_struct_ty {
                BasicTypeEnum::StructType(sty) => {
                    let loaded = self
                        .builder
                        .build_load(sty, sret, &format!("{}_sret_load", type_name))
                        .map_err(|e| CompileError::LlvmError(format!("sret load error: {}", e)))?;
                    loaded.into_struct_value()
                }
                _ => {
                    return Err(CompileError::LlvmError(format!(
                        "c_layout_return_type for '{}' is not a struct",
                        type_name
                    )))
                }
            }
        } else {
            let ret = call_try_basic_value(call).ok_or_else(|| {
                CompileError::LlvmError(
                    "extern call for complex repr(C) return returned void".to_string(),
                )
            })?;
            ret.into_struct_value()
        };

        let internal_sty = self
            .type_llvm
            .get(type_name)
            .and_then(|t| match t {
                BasicTypeEnum::StructType(s) => Some(*s),
                _ => None,
            })
            .ok_or_else(|| {
                CompileError::LlvmError(format!(
                    "internal type for repr(C) record '{}' missing",
                    type_name
                ))
            })?;

        // Build the internal struct by inserting fields into an undef value.
        // CG-C4: The internal struct now uses extern field types (i32 for i32 fields),
        // matching the C layout. Use the actual struct field type for insertion.
        let mut agg: inkwell::values::AggregateValueEnum<'ctx> =
            internal_sty.const_named_struct(&[]).into();
        for (fi, f) in fields.iter().enumerate() {
            let c_field = self
                .builder
                .build_extract_value(c_sv, fi as u32, &format!("{}_{}_cfield", type_name, f.name))
                .map_err(|e| CompileError::LlvmError(format!("extract error: {}", e)))?;
            // Convert C field to the internal struct field type (which is extern layout
            // for repr(C) records, so i32 fields are already the correct width).
            let field_ty = internal_sty
                .get_field_type_at_index(fi as u32)
                .ok_or_else(|| CompileError::LlvmError(format!("field {} type missing", fi)))?;
            let internal_field = self.adjust_int_val(c_field, field_ty)?;
            agg = self
                .builder
                .build_insert_value(
                    agg,
                    internal_field,
                    fi as u32,
                    &format!("{}_{}", type_name, f.name),
                )
                .map_err(|e| CompileError::LlvmError(format!("insert error: {}", e)))?;
        }
        let result = agg.into_struct_value();
        self.build_return(Some(&result.as_basic_value_enum()))?;
        Ok(())
    }

    /// Build a C-layout LLVM struct type for a list of record fields (import direction).
    fn c_layout_return_type(
        &self,
        _name: &str,
        fields: &[Field],
    ) -> MimiResult<BasicTypeEnum<'ctx>> {
        let mut field_tys = Vec::new();
        for f in fields {
            let c_ty = types::mimi_type_to_llvm_extern(self.context, &f.ty).ok_or_else(|| {
                CompileError::LlvmError(format!(
                    "cannot map field type for return struct: {}",
                    crate::core::fmt_type(&f.ty)
                ))
            })?;
            field_tys.push(c_ty);
        }
        Ok(BasicTypeEnum::StructType(
            self.context.struct_type(&field_tys, false),
        ))
    }

    /// Deserialize a returned JSON string into a List/Record value.
    fn emit_list_record_return(
        &mut self,
        ef: &crate::ast::ExternFunc,
        call: &CallSiteValue<'ctx>,
        sig: &ExternFnSignature<'ctx>,
    ) -> MimiResult<()> {
        let ret = call_try_basic_value(call)
            .ok_or_else(|| CompileError::LlvmError("extern call returned void".to_string()))?;
        let json_pv = match ret {
            BasicValueEnum::PointerValue(pv) => pv,
            _ => {
                return Err(CompileError::LlvmError(
                    "Json return must be pointer".to_string(),
                ))
            }
        };

        let out_len_alloca = self.build_alloca(self.context.i64_type(), "out_len")?;
        let ret_tag = ef.ret.as_ref().map(elem_type_tag).unwrap_or(0);
        let ret_tag_iv = self.context.i64_type().const_int(ret_tag as u64, false);

        let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let deserialize_fn = if let Some(f) = self.module.get_function("mimi_json_deserialize") {
            f
        } else {
            let fn_ty = i8_ptr_ty.fn_type(
                &[
                    BasicMetadataTypeEnum::PointerType(i8_ptr_ty),
                    BasicMetadataTypeEnum::PointerType(i8_ptr_ty),
                    BasicMetadataTypeEnum::IntType(self.context.i64_type()),
                ],
                false,
            );
            self.module.add_function(
                "mimi_json_deserialize",
                fn_ty,
                Some(inkwell::module::Linkage::External),
            )
        };

        let data_ptr_val = self
            .build_call(
                deserialize_fn,
                &[
                    BasicMetadataValueEnum::PointerValue(json_pv),
                    BasicMetadataValueEnum::PointerValue(out_len_alloca),
                    BasicMetadataValueEnum::IntValue(ret_tag_iv),
                ],
                "json_deser",
            )?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("json deserialize returned void".to_string()))?
            .into_pointer_value();

        let len_val = self.build_load(self.context.i64_type(), out_len_alloca, "list_len")?;
        if let Some(free_fn) = self.module.get_function("free") {
            self.build_call(
                free_fn,
                &[BasicMetadataValueEnum::PointerValue(json_pv)],
                "free_json_ret",
            )?;
        }

        let list_alloca = self.build_alloca(sig.list_struct_ty, "list_ret")?;
        let len_gep = self
            .gep()
            .build_struct_gep(sig.list_struct_ty, list_alloca, 0, "list_len_gep")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(len_gep, len_val)?;
        let data_gep = self
            .gep()
            .build_struct_gep(sig.list_struct_ty, list_alloca, 1, "list_data_gep")
            .map_err(|e| CompileError::LlvmError(format!("gep error: {}", e)))?;
        self.build_store(data_gep, data_ptr_val)?;
        let ret_val = self.build_load(sig.list_struct_ty, list_alloca, "list_ret_val")?;
        self.build_return(Some(&ret_val))?;
        Ok(())
    }

    /// Deserialize a returned JSON string back into a Mimi tuple struct.
    fn emit_tuple_return(
        &mut self,
        ef: &crate::ast::ExternFunc,
        call: &CallSiteValue<'ctx>,
    ) -> MimiResult<()> {
        let tuple_ty = ef.ret.as_ref().ok_or_else(|| {
            CompileError::LlvmError("expected tuple return type for extern function".to_string())
        })?;
        let elems = match tuple_ty {
            crate::ast::Type::Tuple(e) => e,
            _ => return Err(CompileError::LlvmError("expected tuple type".to_string())),
        };
        let n_elems = elems.len();
        let i64_ty = self.context.i64_type();
        let i32_ty = self.context.i32_type();
        let zero = i32_ty.const_int(0, false);

        let ret = call_try_basic_value(call)
            .ok_or_else(|| CompileError::LlvmError("extern call returned void".to_string()))?;
        let json_pv = match ret {
            BasicValueEnum::PointerValue(pv) => pv,
            _ => {
                return Err(CompileError::LlvmError(
                    "tuple return must be pointer".to_string(),
                ))
            }
        };

        let arr_ty = i64_ty.array_type(n_elems as u32);
        let tys_alloca = self.build_alloca(BasicTypeEnum::ArrayType(arr_ty), "tuple_ret_tys")?;
        for (ei, elem_ty) in elems.iter().enumerate() {
            let tag_val = elem_type_tag(elem_ty);
            let tag_i64 = i64_ty.const_int(tag_val as u64, false);
            let idx = i32_ty.const_int(ei as u64, false);
            let ty_gep = self
                .gep()
                .build_gep(
                    i64_ty,
                    tys_alloca,
                    &[zero, idx],
                    &format!("tuple_ret_ty_gep_{}", ei),
                )
                .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
            self.build_store(ty_gep, tag_i64)?;
        }

        let out_alloca = self.build_alloca(BasicTypeEnum::ArrayType(arr_ty), "tuple_ret_vals")?;
        let n_i64 = i64_ty.const_int(n_elems as u64, false);
        let deser_fn = if let Some(f) = self.module.get_function("mimi_tuple_deserialize") {
            f
        } else {
            let i64_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
            let i8_p_ty = self.context.ptr_type(inkwell::AddressSpace::default());
            let fn_ty = i64_ty.fn_type(
                &[
                    BasicMetadataTypeEnum::PointerType(i8_p_ty),
                    BasicMetadataTypeEnum::IntType(i64_ty),
                    BasicMetadataTypeEnum::PointerType(i64_ptr_ty),
                    BasicMetadataTypeEnum::PointerType(i64_ptr_ty),
                ],
                false,
            );
            self.module.add_function(
                "mimi_tuple_deserialize",
                fn_ty,
                Some(inkwell::module::Linkage::External),
            )
        };

        self.build_call(
            deser_fn,
            &[
                BasicMetadataValueEnum::PointerValue(json_pv),
                BasicMetadataValueEnum::IntValue(n_i64),
                BasicMetadataValueEnum::PointerValue(tys_alloca),
                BasicMetadataValueEnum::PointerValue(out_alloca),
            ],
            "tuple_deser",
        )?;

        if let Some(free_fn) = self.module.get_function("free") {
            self.build_call(
                free_fn,
                &[BasicMetadataValueEnum::PointerValue(json_pv)],
                "free_json_ret",
            )?;
        }

        let struct_ty = types::mimi_type_to_llvm(self.context, tuple_ty)
            .ok_or_else(|| CompileError::LlvmError("unsupported tuple type".to_string()))?;
        let struct_alloca = self.build_alloca(struct_ty, "tuple_ret")?;
        for (ei, elem_ty) in elems.iter().enumerate() {
            let idx = i32_ty.const_int(ei as u64, false);
            let val_gep = self
                .gep()
                .build_gep(
                    i64_ty,
                    out_alloca,
                    &[zero, idx],
                    &format!("tuple_ret_val_gep_{}", ei),
                )
                .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
            let raw_i64 = self
                .build_load(i64_ty, val_gep, &format!("tuple_ret_raw_{}", ei))?
                .into_int_value();

            let elem_llvm_ty = types::mimi_type_to_llvm(self.context, elem_ty)
                .unwrap_or(BasicTypeEnum::IntType(i64_ty));
            let field_val: BasicValueEnum = match elem_llvm_ty {
                BasicTypeEnum::FloatType(ft) => {
                    let tmp_alloca = self
                        .build_alloca(BasicTypeEnum::IntType(i64_ty), &format!("f_tmp_{}", ei))?;
                    self.build_store(tmp_alloca, raw_i64)?;
                    let cast = self
                        .builder
                        .build_pointer_cast(
                            tmp_alloca,
                            self.context.ptr_type(inkwell::AddressSpace::default()),
                            &format!("f_cast_{}", ei),
                        )
                        .map_err(|e| CompileError::LlvmError(format!("cast: {}", e)))?;
                    self.build_load(BasicTypeEnum::FloatType(ft), cast, &format!("f_val_{}", ei))?
                }
                BasicTypeEnum::IntType(it) => BasicValueEnum::IntValue(
                    self.builder
                        .build_int_cast(raw_i64, it, &format!("i_cast_{}", ei))
                        .map_err(|e| CompileError::LlvmError(format!("cast: {}", e)))?,
                ),
                BasicTypeEnum::PointerType(_pt) => {
                    let i8_ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                    let int_to_ptr = self
                        .builder
                        .build_int_to_ptr(raw_i64, i8_ptr_ty, &format!("ptr_cast_{}", ei))
                        .map_err(|e| CompileError::LlvmError(format!("int to ptr: {}", e)))?;
                    BasicValueEnum::PointerValue(int_to_ptr)
                }
                _ => BasicValueEnum::IntValue(raw_i64),
            };
            let field_gep = self
                .gep()
                .build_struct_gep(
                    struct_ty.into_struct_type(),
                    struct_alloca,
                    ei as u32,
                    &format!("tuple_ret_field_{}", ei),
                )
                .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
            self.build_store(field_gep, field_val)?;
        }
        let ret_val = self.build_load(struct_ty, struct_alloca, "tuple_ret_val")?;
        self.build_return(Some(&ret_val))?;
        Ok(())
    }

    /// Convert a returned C string (`i8*`) into a Mimi `{i8*, i64}` struct.
    fn emit_string_return(
        &mut self,
        call: &CallSiteValue<'ctx>,
        sig: &ExternFnSignature<'ctx>,
    ) -> MimiResult<()> {
        let ret = call_try_basic_value(call)
            .ok_or_else(|| CompileError::LlvmError("extern call returned void".to_string()))?;
        let raw_ptr = match ret {
            BasicValueEnum::PointerValue(pv) => pv,
            _ => {
                return Err(CompileError::LlvmError(
                    "string return must be pointer".to_string(),
                ))
            }
        };
        let strlen_fn = self.get_runtime_fn("strlen")?;
        let len = self
            .build_call(
                strlen_fn,
                &[BasicMetadataValueEnum::PointerValue(raw_ptr)],
                "strlen",
            )?
            .try_as_basic_value_opt()
            .ok_or_else(|| CompileError::LlvmError("strlen returned void".to_string()))?
            .into_int_value();

        let struct_ty = sig.wrapper_ret_ty;
        let alloca = self.build_alloca(struct_ty, "string_ret")?;
        let ptr_gep = self
            .gep()
            .build_struct_gep(struct_ty, alloca, 0, "str_ptr_gep")
            .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
        self.build_store(ptr_gep, raw_ptr)?;
        let len_gep = self
            .gep()
            .build_struct_gep(struct_ty, alloca, 1, "str_len_gep")
            .map_err(|e| CompileError::LlvmError(format!("gep: {}", e)))?;
        self.build_store(len_gep, len)?;
        let ret_val = self.build_load(struct_ty, alloca, "string_ret_val")?;
        self.build_return(Some(&ret_val))?;
        Ok(())
    }
}
